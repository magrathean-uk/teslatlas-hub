// SPDX-License-Identifier: AGPL-3.0-only

//! Scripted service effects for synthetic lifecycle callers only.

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
    time::{Duration, Instant},
};

use crate::r78_lifecycle::{ReadinessBounds, ServiceEffects, ServiceFailure};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Loaded,
    Bootout,
    WaitUnloaded,
    Admit,
    Work,
    Release,
    Bootstrap,
    Kickstart,
    WaitReady,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordingError {
    Rejected,
    UnexpectedStep,
}

#[derive(Clone, Copy, Debug)]
pub enum Reply {
    Done,
    Loaded(bool),
    Rejected,
    Panicked,
}

#[derive(Clone, Copy, Debug)]
pub struct Step {
    pub operation: Operation,
    pub reply: Reply,
    pub elapsed: Duration,
}

impl Step {
    pub fn done(operation: Operation) -> Self {
        Self {
            operation,
            reply: Reply::Done,
            elapsed: Duration::ZERO,
        }
    }

    pub fn loaded(loaded: bool) -> Self {
        Self {
            operation: Operation::Loaded,
            reply: Reply::Loaded(loaded),
            elapsed: Duration::ZERO,
        }
    }

    pub fn rejected(operation: Operation) -> Self {
        Self {
            operation,
            reply: Reply::Rejected,
            elapsed: Duration::ZERO,
        }
    }

    pub fn after(mut self, elapsed: Duration) -> Self {
        self.elapsed = elapsed;
        self
    }

    pub fn panicked(operation: Operation) -> Self {
        Self { operation, reply: Reply::Panicked, elapsed: Duration::ZERO }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Event {
    pub operation: Operation,
    pub deadline: Option<Instant>,
}

pub struct Recording {
    steps: VecDeque<Step>,
    events: Rc<RefCell<Vec<Event>>>,
    clock: Rc<Cell<Instant>>,
    readiness: Option<ReadinessBounds>,
    unwind_restart_failures: Vec<ServiceFailure<RecordingError>>,
    panic_on_unwind_recording: bool,
}

pub struct RecordingAdmission {
    events: Rc<RefCell<Vec<Event>>>,
}

pub struct WorkRecorder {
    events: Rc<RefCell<Vec<Event>>>,
    clock: Rc<Cell<Instant>>,
}

impl WorkRecorder {
    pub fn work_for(&self, duration: Duration) {
        self.events.borrow_mut().push(Event {
            operation: Operation::Work,
            deadline: None,
        });
        self.clock.set(self.clock.get() + duration);
    }
}

impl Drop for RecordingAdmission {
    fn drop(&mut self) {
        self.events.borrow_mut().push(Event {
            operation: Operation::Release,
            deadline: None,
        });
    }
}

impl Recording {
    pub fn new(steps: impl IntoIterator<Item = Step>) -> Self {
        Self {
            steps: steps.into_iter().collect(),
            events: Rc::new(RefCell::new(Vec::new())),
            clock: Rc::new(Cell::new(Instant::now())),
            readiness: None,
            unwind_restart_failures: Vec::new(),
            panic_on_unwind_recording: false,
        }
    }

    pub fn events(&self) -> Vec<Event> {
        self.events.borrow().clone()
    }

    pub fn remaining(&self) -> usize {
        self.steps.len()
    }

    pub fn panic_on_unwind_recording(&mut self) {
        self.panic_on_unwind_recording = true;
    }

    pub fn readiness(&self) -> Option<ReadinessBounds> {
        self.readiness
    }

    pub fn unwind_restart_failures(&self) -> &[ServiceFailure<RecordingError>] {
        &self.unwind_restart_failures
    }

    pub fn work_recorder(&self) -> WorkRecorder {
        WorkRecorder {
            events: Rc::clone(&self.events),
            clock: Rc::clone(&self.clock),
        }
    }

    fn step(&mut self, operation: Operation, deadline: Instant) -> Result<Reply, RecordingError> {
        self.events.borrow_mut().push(Event {
            operation,
            deadline: Some(deadline),
        });
        let step = self
            .steps
            .pop_front()
            .ok_or(RecordingError::UnexpectedStep)?;
        if step.operation != operation {
            return Err(RecordingError::UnexpectedStep);
        }
        self.clock.set(self.clock.get() + step.elapsed);
        match step.reply {
            Reply::Rejected => Err(RecordingError::Rejected),
            Reply::Panicked => panic!("synthetic restart panic"),
            reply => Ok(reply),
        }
    }

    fn done(&mut self, operation: Operation, deadline: Instant) -> Result<(), RecordingError> {
        match self.step(operation, deadline)? {
            Reply::Done => Ok(()),
            _ => Err(RecordingError::UnexpectedStep),
        }
    }
}

impl ServiceEffects for Recording {
    type Admission = RecordingAdmission;
    type Error = RecordingError;

    fn now(&self) -> Instant {
        self.clock.get()
    }

    fn loaded(&mut self, deadline: Instant) -> Result<bool, RecordingError> {
        match self.step(Operation::Loaded, deadline)? {
            Reply::Loaded(value) => Ok(value),
            _ => Err(RecordingError::UnexpectedStep),
        }
    }

    fn bootout(&mut self, deadline: Instant) -> Result<(), RecordingError> {
        self.done(Operation::Bootout, deadline)
    }

    fn wait_unloaded(&mut self, deadline: Instant) -> Result<(), RecordingError> {
        self.done(Operation::WaitUnloaded, deadline)
    }

    fn admit(&mut self, deadline: Instant) -> Result<RecordingAdmission, RecordingError> {
        self.done(Operation::Admit, deadline)?;
        Ok(RecordingAdmission {
            events: Rc::clone(&self.events),
        })
    }

    fn bootstrap(&mut self, deadline: Instant) -> Result<(), RecordingError> {
        self.done(Operation::Bootstrap, deadline)
    }

    fn kickstart(&mut self, deadline: Instant) -> Result<(), RecordingError> {
        self.done(Operation::Kickstart, deadline)
    }

    fn wait_ready(
        &mut self,
        deadline: Instant,
        bounds: ReadinessBounds,
    ) -> Result<(), RecordingError> {
        self.readiness = Some(bounds);
        self.done(Operation::WaitReady, deadline)
    }

    fn record_unwind_restart_failure(&mut self, failure: ServiceFailure<RecordingError>) {
        self.unwind_restart_failures.push(failure);
        if self.panic_on_unwind_recording {
            panic!("synthetic recording hook panic");
        }
    }
}
