//! Process marker for the one local Hub instance.

use std::{path::Path, sync::Arc, time::Duration};

use crate::user_lifetime_lock::UserLifetimeLock;
pub use crate::user_lifetime_lock::UserLifetimeLockError;

/// Non-cloneable owner of the Hub data-directory lock.
#[doc(hidden)]
pub struct AdmittedUserHub {
    lock: UserLifetimeLock,
}

impl std::fmt::Debug for AdmittedUserHub {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AdmittedUserHub")
            .finish_non_exhaustive()
    }
}

impl AdmittedUserHub {
    #[doc(hidden)]
    pub fn admit(data_dir: &Path) -> Result<Arc<Self>, UserLifetimeLockError> {
        let admitted = Arc::new(Self {
            lock: UserLifetimeLock::acquire(data_dir)?,
        });
        admitted.assert_sensitive_access()?;
        Ok(admitted)
    }

    /// Revalidate the retained directory and lock descriptors before network
    /// work.
    #[doc(hidden)]
    pub fn assert_sensitive_access(&self) -> Result<(), UserLifetimeLockError> {
        self.lock.revalidate()
    }

    #[doc(hidden)]
    pub fn assert_store_path(&self, store: &Path) -> Result<(), UserLifetimeLockError> {
        self.lock.require_store_path(store)
    }

    #[doc(hidden)]
    pub async fn wait_until_invalid(&self) -> UserLifetimeLockError {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if let Err(error) = self.assert_sensitive_access() {
                return error;
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(data_dir: &Path) -> Result<Arc<Self>, UserLifetimeLockError> {
        Self::admit(data_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_is_arc_shared_and_checks_exact_store() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let admitted = AdmittedUserHub::for_test(temporary.path()).expect("admit");
        let shared = Arc::clone(&admitted);
        shared
            .assert_store_path(temporary.path())
            .expect("same store");
        assert!(
            admitted
                .assert_store_path(&temporary.path().join("other"))
                .is_err()
        );
        assert_eq!(Arc::strong_count(&admitted), 2);
    }
}
