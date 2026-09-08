use crate::{MediaError, MediaLimits, Result, flv::FlvTag};
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};

#[derive(Clone)]
pub struct PacketSender {
    sender: mpsc::Sender<QueuedTag>,
    budget: Arc<Semaphore>,
}
pub struct PacketReceiver {
    receiver: mpsc::Receiver<QueuedTag>,
}
pub struct QueuedTag {
    tag: Arc<FlvTag>,
    _permit: OwnedSemaphorePermit,
}
impl QueuedTag {
    pub fn tag(&self) -> &FlvTag {
        &self.tag
    }
}

pub fn bounded(limits: &MediaLimits) -> Result<(PacketSender, PacketReceiver)> {
    if limits.queue_bytes == 0
        || limits.queue_bytes > u32::MAX as usize
        || limits.queue_bytes > Semaphore::MAX_PERMITS
        || limits.queue_events == 0
        || limits.queue_events > 65536
    {
        return Err(MediaError::InvalidConfiguration);
    }
    let (sender, receiver) = mpsc::channel(limits.queue_events);
    Ok((
        PacketSender {
            sender,
            budget: Arc::new(Semaphore::new(limits.queue_bytes)),
        },
        PacketReceiver { receiver },
    ))
}
impl PacketSender {
    pub fn try_send(&self, tag: Arc<FlvTag>) -> Result<()> {
        let size = u32::try_from(tag.wire_len()).map_err(|_| MediaError::Backpressure)?;
        let permit = self
            .budget
            .clone()
            .try_acquire_many_owned(size)
            .map_err(|_| MediaError::Backpressure)?;
        self.sender
            .try_send(QueuedTag {
                tag,
                _permit: permit,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Closed(_) => MediaError::Cancelled,
                mpsc::error::TrySendError::Full(_) => MediaError::Backpressure,
            })
    }
}
impl PacketReceiver {
    pub async fn recv(&mut self) -> Option<QueuedTag> {
        self.receiver.recv().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn slow_target_cannot_consume_another_targets_budget() {
        let limits = MediaLimits {
            queue_events: 1,
            queue_bytes: 20,
            ..Default::default()
        };
        let (slow, _blocked) = bounded(&limits).unwrap();
        let (fast, mut receiver) = bounded(&limits).unwrap();
        let packet = Arc::new(FlvTag::new(8, 0, Arc::from(&b"\xaf\x00ab"[..]), 10).unwrap());
        slow.try_send(packet.clone()).unwrap();
        assert_eq!(slow.try_send(packet.clone()), Err(MediaError::Backpressure));
        fast.try_send(packet.clone()).unwrap();
        let held = receiver.recv().await.unwrap();
        assert_eq!(fast.try_send(packet.clone()), Err(MediaError::Backpressure));
        drop(held);
        fast.try_send(packet).unwrap();
    }
}
