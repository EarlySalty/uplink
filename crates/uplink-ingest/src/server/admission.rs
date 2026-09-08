use std::sync::{Arc, Mutex};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Erst nach erfolgreicher Identitätsprüfung belegt. Medienevents behalten
/// denselben Anteil bis zum letzten Verbraucher, unabhängig vom TCP-Ende.
pub(super) struct SessionSlot(Mutex<Option<OwnedSemaphorePermit>>);

impl SessionSlot {
    pub(super) fn empty() -> Self {
        Self(Mutex::new(None))
    }

    pub(super) fn authorize(&self, sessions: &Arc<Semaphore>) -> Result<(), ()> {
        let permit = sessions.clone().try_acquire_owned().map_err(|_| ())?;
        let mut slot = self.0.lock().map_err(|_| ())?;
        if slot.is_some() {
            return Err(());
        }
        *slot = Some(permit);
        Ok(())
    }
}
