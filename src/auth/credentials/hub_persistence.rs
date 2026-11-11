// SPDX-License-Identifier: AGPL-3.0-only
use super::*;
use uuid::Uuid;
use crate::db::{OutboundRequestReceiptId, OutboundRequestOutcome, RefreshCompletion};

/// The manager cache is initialised from the pair its constructor loaded.
/// Clones shared with detached jobs never load or compute a replacement cache.
pub(super) struct HubCredentialPersistence {
    pub(super) vault: Arc<CredentialVault>,
    generation: Mutex<Uuid>,
    #[cfg(test)] pub(super) faults: Mutex<std::collections::VecDeque<PersistFault>>,
    #[cfg(test)] pub(super) closing_failure: AtomicBool,
    #[cfg(test)] pub(super) closing_failures: std::sync::atomic::AtomicUsize,
    #[cfg(test)] pub(super) reservation_failures: std::sync::atomic::AtomicUsize,
    #[cfg(test)] pub(super) closing_calls: std::sync::atomic::AtomicUsize,
}
impl HubCredentialPersistence {
    pub(super) fn new(vault: Arc<CredentialVault>, generation: Uuid) -> Self {
        Self { vault, generation: Mutex::new(generation),
            #[cfg(test)] faults: Mutex::new(std::collections::VecDeque::new()),
            #[cfg(test)] closing_failure: AtomicBool::new(false),
            #[cfg(test)] closing_failures: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)] reservation_failures: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)] closing_calls: std::sync::atomic::AtomicUsize::new(0) }
    }
    pub(super) fn generation(&self) -> Uuid {
        *self.generation.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    pub(super) fn set_generation(&self, generation: Uuid) {
        *self.generation.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = generation;
    }
    pub(super) fn stored_generation(&self) -> Result<Option<Uuid>, CredentialError> {
        self.vault.stored_generation(CredentialSlot::OwnerApi).map_err(Into::into)
    }
    pub(super) fn relation(&self) -> Result<StoredPairRelation, CredentialError> {
        let stored = self.stored_generation()?;
        Ok(match stored {
            None => StoredPairRelation::Missing,
            Some(value) if value == self.generation() => StoredPairRelation::Current,
            Some(value) if self.vault.is_completed_output(CredentialSlot::OwnerApi, value).map_err(CredentialError::from)? => StoredPairRelation::OwnOutput(value),
            Some(_) => StoredPairRelation::External,
        })
    }
    pub(super) fn begin_refresh(&self) -> Result<(OutboundRequestReceiptId, Uuid), CredentialError> {
        #[cfg(test)] if Self::consume_failure(&self.reservation_failures) { return Err(CredentialError::LegacyTokenStateWrite); }
        let generation = self.generation();
        self.vault.begin_refresh(CredentialSlot::OwnerApi, generation).map(|receipt| (receipt, generation)).map_err(Into::into)
    }
    pub(super) fn cancel_refresh(&self, receipt: OutboundRequestReceiptId, input: Uuid) -> Result<(), CredentialError> {
        #[cfg(test)] self.closing_calls.fetch_add(1, Ordering::SeqCst);
        #[cfg(test)] if Self::consume_failure(&self.closing_failures) { return Err(CredentialError::LegacyTokenStateWrite); }
        self.vault.cancel_refresh(receipt, input).map_err(Into::into)
    }
    pub(super) fn close_refresh(&self, receipt: OutboundRequestReceiptId, input: Uuid, outcome: OutboundRequestOutcome, status: Option<u16>, retry_after: Option<u64>, release: bool) -> Result<(), CredentialError> {
        #[cfg(test)] self.closing_calls.fetch_add(1, Ordering::SeqCst);
        #[cfg(test)] if self.closing_failure.load(Ordering::SeqCst) || Self::consume_failure(&self.closing_failures) { return Err(CredentialError::LegacyTokenStateWrite); }
        self.vault.close_refresh(receipt, input, outcome, status, retry_after, release).map_err(Into::into)
    }
    pub(super) fn persist_refreshed(&self, receipt: OutboundRequestReceiptId, input: Uuid, output: Uuid, access: &str, refresh: &str, expires: i64, next: i64) -> Result<RefreshCompletion, CredentialError> {
        #[cfg(test)] let fault = self.faults.lock().unwrap().pop_front();
        #[cfg(test)] if fault == Some(PersistFault::Fail) { return Err(CredentialError::LegacyTokenStateWrite); }
        let result = self.vault.persist_refreshed(CredentialSlot::OwnerApi, receipt, input, output, access, refresh, expires, next).map_err(Into::into);
        #[cfg(test)] if result.is_ok() && fault == Some(PersistFault::Ambiguous) { return Err(CredentialError::LegacyTokenStateWrite); }
        result
    }
    #[cfg(test)] fn consume_failure(count: &std::sync::atomic::AtomicUsize) -> bool {
        let mut value=count.load(Ordering::SeqCst);
        while value>0 {match count.compare_exchange_weak(value,value-1,Ordering::SeqCst,Ordering::SeqCst){Ok(_)=>return true,Err(current)=>value=current}}
        false
    }
}
