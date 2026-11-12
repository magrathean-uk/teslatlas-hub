// SPDX-License-Identifier: AGPL-3.0-only

use std::{cell::{Cell, RefCell}, panic::{AssertUnwindSafe, catch_unwind}, path::Path, rc::Rc, sync::{Arc, atomic::{AtomicBool, Ordering}}, time::Instant};
use crate::{CredentialVaultEntry, CredentialVaultEntryError, db::HubStore, hub_user_process::AdmittedUserHub,
    platform::r78_lifecycle::{Completion, PrimaryFailure, ReadinessBounds, ServiceBudgets, ServiceEffects, ServiceFailure, Stage, with_transient_pause}};
use super::{CredentialCommandError, SignInScope};

#[derive(Debug)]
pub enum SignInServiceError<E> { Backend(E), AdmissionNotReleased, Panicked }
#[derive(Debug)]
pub enum SignInWorkError<W> { Open(CredentialCommandError), Work(W), AdmissionNotReleased, Panicked }

struct Resources {
    admission: Option<Arc<AdmittedUserHub>>,
    scope: Option<SignInScope>,
    release_failed: Arc<AtomicBool>,
}
impl Resources {
    fn release(&mut self) {
        if let Some(scope) = self.scope.take()
            && scope.release().is_err() { self.release_failed.store(true, Ordering::Release); }
        if let Some(admission) = self.admission.take() {
            match Arc::try_unwrap(admission) {
                Ok(admission) => drop(admission),
                Err(_) => self.release_failed.store(true, Ordering::Release),
            }
        }
    }
}
struct Release(Rc<RefCell<Resources>>);
impl Drop for Release { fn drop(&mut self) { self.0.borrow_mut().release(); } }

fn effect<T, E>(call: impl FnOnce() -> Result<T, E>) -> Result<T, SignInServiceError<E>> {
    match catch_unwind(AssertUnwindSafe(call)) {
        Ok(result) => result.map_err(SignInServiceError::Backend),
        Err(_) => Err(SignInServiceError::Panicked),
    }
}
struct Effects<'a, E: ServiceEffects> {
    backend: &'a mut E,
    resources: Rc<RefCell<Resources>>,
    admitted: Cell<bool>,
    loaded_queries: Cell<usize>,
    initially_loaded: Cell<bool>,
    stage: Rc<Cell<Stage>>,
    unwind_failure: Rc<RefCell<Option<ServiceFailure<SignInServiceError<E::Error>>>>>,
}
impl<E: ServiceEffects<Admission = Arc<AdmittedUserHub>>> ServiceEffects for Effects<'_, E> {
    type Admission = Release;
    type Error = SignInServiceError<E::Error>;
    fn now(&self) -> Instant { self.backend.now() }
    fn loaded(&mut self, deadline: Instant) -> Result<bool, Self::Error> {
        let first = self.loaded_queries.get() == 0;
        self.loaded_queries.set(self.loaded_queries.get() + 1);
        self.stage.set(if first { Stage::InitialQuery } else { Stage::RestartQuery });
        if self.admitted.get() && self.resources.borrow().release_failed.load(Ordering::Acquire) { return Err(SignInServiceError::AdmissionNotReleased); }
        let result = effect(|| self.backend.loaded(deadline));
        if first { self.initially_loaded.set(matches!(&result, Ok(true))); }
        result
    }
    fn bootout(&mut self, deadline: Instant) -> Result<(), Self::Error> { self.stage.set(Stage::Bootout); effect(|| self.backend.bootout(deadline)) }
    fn wait_unloaded(&mut self, deadline: Instant) -> Result<(), Self::Error> { self.stage.set(Stage::Unload); effect(|| self.backend.wait_unloaded(deadline)) }
    fn admit(&mut self, deadline: Instant) -> Result<Release, Self::Error> {
        self.stage.set(Stage::Admission);
        let admission = effect(|| self.backend.admit(deadline))?;
        self.resources.borrow_mut().admission = Some(admission);
        self.admitted.set(true);
        Ok(Release(Rc::clone(&self.resources)))
    }
    fn bootstrap(&mut self, deadline: Instant) -> Result<(), Self::Error> { self.stage.set(Stage::Bootstrap); effect(|| self.backend.bootstrap(deadline)) }
    fn kickstart(&mut self, deadline: Instant) -> Result<(), Self::Error> { self.stage.set(Stage::Kickstart); effect(|| self.backend.kickstart(deadline)) }
    fn wait_ready(&mut self, deadline: Instant, bounds: ReadinessBounds) -> Result<(), Self::Error> { self.stage.set(Stage::Readiness); effect(|| self.backend.wait_ready(deadline, bounds)) }
    fn record_unwind_restart_failure(&mut self, failure: ServiceFailure<Self::Error>) { *self.unwind_failure.borrow_mut() = Some(failure); }
}

pub type SignInCompletion<T, E, W> = Completion<T, SignInServiceError<E>, SignInWorkError<W>>;

fn run<E, T, W>(
    backend: &mut E, budgets: ServiceBudgets, data_dir: &Path,
    open: impl FnOnce(HubStore, Arc<AdmittedUserHub>, &Path) -> Result<CredentialVaultEntry, CredentialVaultEntryError>,
    work: impl FnOnce(&CredentialVaultEntry) -> Result<T, W>,
) -> SignInCompletion<T, E::Error, W>
where E: ServiceEffects<Admission = Arc<AdmittedUserHub>> {
    let release_failed = Arc::new(AtomicBool::new(false));
    let resources = Rc::new(RefCell::new(Resources { admission: None, scope: None, release_failed: Arc::clone(&release_failed) }));
    let unwind_failure = Rc::new(RefCell::new(None));
    let stage = Rc::new(Cell::new(Stage::InitialQuery));
    let mut effects = Effects { backend, resources: Rc::clone(&resources), admitted: Cell::new(false), loaded_queries: Cell::new(0), initially_loaded: Cell::new(false), stage: Rc::clone(&stage), unwind_failure: Rc::clone(&unwind_failure) };
    let work_result = RefCell::new(None);
    let completion = catch_unwind(AssertUnwindSafe(|| with_transient_pause(&mut effects, budgets, |_| {
        let result = catch_unwind(AssertUnwindSafe(|| {
            let admission = resources.borrow_mut().admission.take().ok_or_else(|| SignInWorkError::Open(CredentialCommandError::new("sign_in_service_pause_failed")))?;
            let scope = SignInScope::from_admission_using(data_dir, admission, open, Arc::clone(&release_failed)).map_err(|error| {
                let error = match error.downcast::<CredentialCommandError>() {
                    Ok(error) => *error,
                    Err(_) => CredentialCommandError::new("credential_store_unavailable"),
                };
                SignInWorkError::Open(error)
            })?;
            resources.borrow_mut().scope = Some(scope);
            let resources = resources.borrow();
            work(resources.scope.as_ref().expect("admitted sign-in scope").entry()).map_err(SignInWorkError::Work)
        }));
        let result = match result { Ok(result) => result, Err(_) => Err(SignInWorkError::Panicked) };
        let failed = result.is_err();
        *work_result.borrow_mut() = Some(result);
        if failed { Err(()) } else { Ok(()) }
    })));
    let original = |result| match result {
        Ok(()) | Err(PrimaryFailure::Work(())) => work_result.borrow_mut().take().unwrap_or(Err(SignInWorkError::Panicked)).map_err(PrimaryFailure::Work),
        Err(PrimaryFailure::Service(error)) => Err(PrimaryFailure::Service(error)),
    };
    let completion = match completion {
        Ok(Completion::Finished(result)) => Completion::Finished(original(result)),
        Ok(Completion::RestartFailed { original: result, restart }) => Completion::RestartFailed { original: original(result), restart },
        Err(_) => {
            let original = work_result.borrow_mut().take().unwrap_or(Err(SignInWorkError::Panicked)).map_err(PrimaryFailure::Work);
            if effects.initially_loaded.get() {
                let restart = unwind_failure.borrow_mut().take().unwrap_or(ServiceFailure::Backend { stage: stage.get(), error: SignInServiceError::Panicked });
                Completion::RestartFailed { original, restart }
            } else { Completion::Finished(original) }
        }
    };
    if release_failed.load(Ordering::Acquire) {
        match completion {
            Completion::Finished(Ok(_)) => Completion::Finished(Err(PrimaryFailure::Work(SignInWorkError::AdmissionNotReleased))),
            other => other,
        }
    } else { completion }
}

pub fn with_sign_in_scope<E, T, W>(backend: &mut E, budgets: ServiceBudgets, data_dir: &Path, work: impl FnOnce(&CredentialVaultEntry) -> Result<T, W>) -> SignInCompletion<T, E::Error, W>
where E: ServiceEffects<Admission = Arc<AdmittedUserHub>> {
    run(backend, budgets, data_dir, crate::open_credential_vault_for_entry, work)
}

#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn with_sign_in_scope_for_test<E, T, W>(backend: &mut E, budgets: ServiceBudgets, data_dir: &Path, excluded: bool, work: impl FnOnce(&CredentialVaultEntry) -> Result<T, W>) -> SignInCompletion<T, E::Error, W>
where E: ServiceEffects<Admission = Arc<AdmittedUserHub>> {
    run(backend, budgets, data_dir, |store, admission, data_dir| CredentialVaultEntry::open_for_test(store, admission, data_dir, excluded), work)
}
