// SPDX-License-Identifier: AGPL-3.0-only

// Test-only support of the supervised loops' seam (B2 fix round, P2, second pass):
// acknowledged gates at chosen points of the supervisors, and a cadence whose clock and
// timers a test controls. Nothing here is part of a production build: the hooks in the
// loops are empty functions there.

/// A point of a supervisor where a test holds the task that reaches it. The task counts
/// its arrival (the test's acknowledgement that the point was reached) and goes on only
/// when the test releases it; a task whose wait is dropped is counted as cancelled.
#[cfg(test)]
struct TestGate {
    arrivals: watch::Sender<usize>,
    permits: tokio::sync::Semaphore,
    cancelled: AtomicUsize,
}

#[cfg(test)]
struct GateCancelGuard<'a> {
    gate: &'a TestGate,
    armed: bool,
}

#[cfg(test)]
impl Drop for GateCancelGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.gate.cancelled.fetch_add(1, Ordering::SeqCst);
        }
    }
}

#[cfg(test)]
impl TestGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            arrivals: watch::channel(0).0,
            permits: tokio::sync::Semaphore::new(0),
            cancelled: AtomicUsize::new(0),
        })
    }

    /// The waits that were dropped before they were released.
    fn cancelled(&self) -> usize {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Let `count` waiting (or later) arrivals through.
    fn release(&self, count: usize) {
        self.permits.add_permits(count);
    }

    /// Let every arrival through from now on.
    fn open(&self) {
        self.permits.add_permits(1 << 20);
    }

    /// Wait (bounded) until `count` tasks have reached the point.
    async fn arrived(&self, label: &str, count: usize) {
        let mut arrivals = self.arrivals.subscribe();
        match timeout(
            Duration::from_secs(30),
            arrivals.wait_for(|arrived| *arrived >= count),
        )
        .await
        {
            Ok(Ok(_)) => {}
            _ => panic!("{label}: the point was not reached {count} time(s)"),
        }
    }

    /// Wait (bounded) until `count` waits were dropped.
    async fn cancelled_at_least(&self, label: &str, count: usize) {
        timeout(Duration::from_secs(30), async {
            while self.cancelled() < count {
                tokio::task::yield_now().await;
                sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{label}: no wait was dropped"));
    }
}

/// Hold the task that reaches `point` if a test armed it; nothing in production.
#[cfg(not(test))]
async fn supervised_test_gate(_point: &'static str) {}

#[cfg(test)]
async fn supervised_test_gate(point: &'static str) {
    let gate = SUPERVISED_COLLECTOR_TEST_SEAM
        .try_with(|seam| seam.gates.lock().expect("gates").get(point).cloned())
        .ok()
        .flatten();
    let Some(gate) = gate else {
        return;
    };
    gate.arrivals.send_modify(|arrived| *arrived += 1);
    let mut guard = GateCancelGuard {
        gate: &gate,
        armed: true,
    };
    gate.permits
        .acquire()
        .await
        .expect("the gate is never closed")
        .forget();
    guard.armed = false;
}

/// The supervisor registers the loss fence it made, for the tests that set it at a
/// chosen point; nothing in production.
#[cfg(not(test))]
fn supervised_test_register_fence(_fence: &AdmissionFence) {}

#[cfg(test)]
fn supervised_test_register_fence(fence: &AdmissionFence) {
    let _ = SUPERVISED_COLLECTOR_TEST_SEAM.try_with(|seam| {
        *seam.admission_fence.lock().expect("admission fence") = Some(fence.clone());
    });
}

/// The supervisor registers the audit of the write guard it made, for the tests that
/// read what the run's store boundaries were told; nothing in production. Only the
/// audit is kept: the guard holds the retained admission.
#[cfg(not(test))]
fn supervised_test_register_guard(_guard: &RunWriteGuard) {}

#[cfg(test)]
fn supervised_test_register_guard(guard: &RunWriteGuard) {
    let _ = SUPERVISED_COLLECTOR_TEST_SEAM.try_with(|seam| {
        *seam.write_audit.lock().expect("write audit") = Some(Arc::clone(&guard.audit));
        if let Some((point, nth, action)) = seam.write_trip.lock().expect("write trip").clone() {
            guard.audit.trip_at_nth(point, nth, move || action());
        }
    });
}

/// A future that runs with the caller's test seam: the seam is a task-local, and a task
/// spawned by the supervisor does not inherit it. Nothing in production.
#[cfg(not(test))]
fn supervised_test_scoped<F: Future>(future: F) -> F {
    future
}

#[cfg(test)]
fn supervised_test_scoped<F: Future>(future: F) -> impl Future<Output = F::Output> {
    let seam = SUPERVISED_COLLECTOR_TEST_SEAM.try_with(Arc::clone).ok();
    async move {
        match seam {
            Some(seam) => SUPERVISED_COLLECTOR_TEST_SEAM.scope(seam, future).await,
            None => future.await,
        }
    }
}

#[cfg(test)]
impl SupervisedCollectorTestSeam {
    /// Arm `point`: from now on a task that reaches it waits for the returned gate.
    fn arm_gate(&self, point: &'static str) -> Arc<TestGate> {
        Arc::clone(
            self.gates
                .lock()
                .expect("gates")
                .entry(point)
                .or_insert_with(TestGate::new),
        )
    }

    /// Build the loops' scheduler on `clock` instead of the system clock.
    fn select_scheduler_clock(&self, clock: &Arc<ManualClock>) {
        *self.scheduler_clock.lock().expect("scheduler clock") = Some(Arc::clone(clock));
    }

    /// Control the cadence of the loop: its scheduler is built on `clock`, and its
    /// timers and iteration instants follow the controller.
    fn control_cadence(&self, clock: &Arc<ManualClock>) -> Arc<CadenceControl> {
        self.select_scheduler_clock(clock);
        let control = CadenceControl::new(Arc::clone(clock));
        *self.cadence.lock().expect("cadence") = Some(Arc::clone(&control));
        control
    }
}

/// The clock the loops' scheduler is built on when a test selected one.
#[cfg(not(test))]
fn supervised_test_scheduler_clock() -> Option<Arc<dyn CollectorClock>> {
    None
}

#[cfg(test)]
fn supervised_test_scheduler_clock() -> Option<Arc<dyn CollectorClock>> {
    SUPERVISED_COLLECTOR_TEST_SEAM
        .try_with(|seam| seam.scheduler_clock.lock().expect("scheduler clock").clone())
        .ok()
        .flatten()
        .map(|clock| clock as Arc<dyn CollectorClock>)
}

/// The instant the loops measure their own cadence on (iteration starts, the ordinary
/// sleep and the idle floor): the monotonic timer, or the controlled cadence's clock
/// when a test controls it.
#[cfg(not(test))]
fn loop_now() -> Instant {
    Instant::now()
}

#[cfg(test)]
fn loop_now() -> Instant {
    SUPERVISED_COLLECTOR_TEST_SEAM
        .try_with(|seam| seam.cadence.lock().expect("cadence").clone())
        .ok()
        .flatten()
        .map_or_else(Instant::now, |control| control.clock.mono_now())
}

/// The loops' timer for their cadence: a sleep, or the controlled cadence's wait when a
/// test controls it.
#[cfg(not(test))]
async fn cadence_sleep(duration: Duration) {
    sleep(duration).await;
}

#[cfg(test)]
async fn cadence_sleep(duration: Duration) {
    let control = SUPERVISED_COLLECTOR_TEST_SEAM
        .try_with(|seam| seam.cadence.lock().expect("cadence").clone())
        .ok()
        .flatten();
    match control {
        Some(control) => control.wait(duration).await,
        None => sleep(duration).await,
    }
}

/// What a loop under a controlled cadence has reached and is held at.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
enum CadenceEvent {
    /// A tick, its commit included, is complete (the loop waits to be resumed).
    Tick(TickCheckpoint),
    /// The loop asks for a timer of `duration` at the instant `at` of the clock (it
    /// waits until the test has moved the clock and resumed it).
    Wait { at: Instant, duration: Duration },
}

/// The controlled cadence of one loop (B2 fix round, P2, second pass; C3 clarification
/// 3, CT55b). The loop's scheduler reads the controller's clock, and its timers (the
/// planned wait, the idle floor) and its completed ticks stop at the controller: each
/// is posted as an event and the loop waits until the test resumes it, after it has
/// moved the clock by the duration the production code asked for. The requests, the
/// commits and the replies of the double run for real while the clock is still, so the
/// measurement is of the logical time the loop's own waiting rule produced and nothing
/// a loaded machine does can change it. Waits of the loop's streams (the drain
/// deadlines) stay on the runtime's timer; the beds that use this have none.
#[cfg(test)]
struct CadenceControl {
    clock: Arc<ManualClock>,
    events: tokio::sync::Mutex<mpsc::UnboundedReceiver<CadenceEvent>>,
    posted: mpsc::UnboundedSender<CadenceEvent>,
    resume: tokio::sync::Semaphore,
}

#[cfg(test)]
impl CadenceControl {
    fn new(clock: Arc<ManualClock>) -> Arc<Self> {
        let (posted, events) = mpsc::unbounded_channel();
        Arc::new(Self {
            clock,
            events: tokio::sync::Mutex::new(events),
            posted,
            resume: tokio::sync::Semaphore::new(0),
        })
    }

    /// The loop's side: post `event`, then wait to be resumed.
    async fn hold_at(&self, event: CadenceEvent) {
        let _ = self.posted.send(event);
        self.resume
            .acquire()
            .await
            .expect("the cadence is never closed")
            .forget();
    }

    async fn wait(&self, duration: Duration) {
        let at = self.clock.mono_now();
        self.hold_at(CadenceEvent::Wait { at, duration }).await;
    }

    async fn completed_tick(&self, checkpoint: TickCheckpoint) {
        self.hold_at(CadenceEvent::Tick(checkpoint)).await;
    }

    /// The test's side: the next event the loop is held at. The loop is held until
    /// [`Self::resume`].
    async fn next(&self) -> CadenceEvent {
        let mut events = self.events.lock().await;
        match timeout(Duration::from_secs(30), events.recv()).await {
            Ok(Some(event)) => event,
            _ => panic!("the loop reached neither a completed tick nor a timer"),
        }
    }

    /// Let the loop go on from the event it is held at.
    fn resume(&self) {
        self.resume.add_permits(1);
    }

    fn clock(&self) -> &Arc<ManualClock> {
        &self.clock
    }
}

/// A future that logs, the first time its inner future is left pending, that loop work
/// was awaited (see [`LoopEvent::AwaitedLoopWork`]).
#[cfg(test)]
struct PendingLogged<'a, F> {
    inner: std::pin::Pin<&'a mut F>,
    logged: bool,
}

#[cfg(test)]
impl<'a, F> PendingLogged<'a, F> {
    fn new(inner: std::pin::Pin<&'a mut F>) -> Self {
        Self {
            inner,
            logged: false,
        }
    }
}

#[cfg(test)]
impl<F: Future> Future for PendingLogged<'_, F> {
    type Output = F::Output;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let polled = self.inner.as_mut().poll(context);
        if polled.is_pending() && !self.logged {
            self.logged = true;
            supervised_test_log(LoopEvent::AwaitedLoopWork);
        }
        polled
    }
}
