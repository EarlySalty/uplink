use crate::nachricht::{Ereignis, Rahmen};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    sync::Mutex,
    time::{Duration, Instant},
};
use tokio::sync::broadcast;
const MAX_EVENTS: usize = 1024;
const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_EVENT: usize = 32 * 1024;
const AGE: Duration = Duration::from_secs(900);
pub(crate) struct Bus {
    inner: Mutex<Inner>,
}
struct Inner {
    tx: broadcast::Sender<Rahmen>,
    next: u64,
    events: VecDeque<(Instant, usize, Rahmen)>,
    seen: HashMap<[u8; 32], [u8; 32]>,
    bytes: usize,
}
impl Bus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(256);
        Self {
            inner: Mutex::new(Inner {
                tx,
                next: 1,
                events: VecDeque::new(),
                seen: HashMap::new(),
                bytes: 0,
            }),
        }
    }
    pub fn is_duplicate(&self, event: &Ereignis) -> bool {
        canonical(event).ok().is_some_and(|bytes| {
            self.inner.lock().expect("Bus").seen.get(&key(event)) == Some(&digest(&bytes))
        })
    }
    pub fn publish(&self, event: Ereignis) -> Result<Option<u64>, &'static str> {
        let bytes = canonical(&event)?;
        if bytes.len() > MAX_EVENT {
            return Err("Chatereignis überschreitet das Größenlimit");
        }
        let mut i = self.inner.lock().expect("Bus");
        i.clean();
        let key = key(&event);
        if i.seen.get(&key) == Some(&digest(&bytes)) {
            return Ok(None);
        }
        if i.seen.len() >= MAX_EVENTS * 2 && !i.seen.contains_key(&key) {
            i.seen.clear();
        }
        i.seen.insert(key, digest(&bytes));
        let id = i.next;
        i.next = i
            .next
            .checked_add(1)
            .ok_or("Chatsequenz ist ausgeschöpft")?;
        let frame = Rahmen {
            id,
            ereignis: event,
        };
        i.bytes += bytes.len();
        i.events
            .push_back((Instant::now(), bytes.len(), frame.clone()));
        i.clean();
        let _ = i.tx.send(frame);
        Ok(Some(id))
    }
    pub fn subscribe(
        &self,
        since: Option<u64>,
    ) -> (broadcast::Receiver<Rahmen>, Vec<Rahmen>, bool) {
        let mut i = self.inner.lock().expect("Bus");
        i.clean();
        let gap = since.is_some_and(|s| {
            s >= i.next
                || i.events
                    .front()
                    .is_some_and(|(_, _, f)| s.saturating_add(1) < f.id)
        });
        let since = if gap { None } else { since };
        (
            i.tx.subscribe(),
            i.events
                .iter()
                .filter(|(_, _, f)| since.is_none_or(|s| f.id > s))
                .map(|(_, _, f)| f.clone())
                .collect(),
            gap,
        )
    }
}
impl Inner {
    fn clean(&mut self) {
        while self
            .events
            .front()
            .is_some_and(|(at, _, _)| at.elapsed() > AGE)
            || self.events.len() > MAX_EVENTS
            || self.bytes > MAX_BYTES
        {
            if let Some((_, len, _)) = self.events.pop_front() {
                self.bytes -= len;
            }
        }
    }
}
fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
fn key(event: &Ereignis) -> [u8; 32] {
    digest(event.dedupe_key().as_bytes())
}
fn canonical(event: &Ereignis) -> Result<Vec<u8>, &'static str> {
    let mut value = serde_json::to_value(event).map_err(|_| "Ereignis ist ungültig")?;
    if let Some(object) = value.as_object_mut() {
        object.remove("hervorhebung");
    }
    serde_json::to_vec(&value).map_err(|_| "Ereignis ist ungültig")
}
#[cfg(test)]
mod tests {
    use super::*;
    fn event(title: &str) -> Ereignis {
        Ereignis::Chat(serde_json::from_value(serde_json::json!({"platform":"youtube","channel_id":"7","channel_login":"example","message_id":"same-id","sender_id":"9","sender_login":"viewer","sender_display":"Viewer","badges":[],"fragments":[{"art":"text","text":title}],"sent_at":"2026-09-08T10:00:00Z","is_action":false,"eigene":false})).unwrap())
    }
    #[test]
    fn same_id_update_is_delivered_but_exact_replay_is_not() {
        let b = Bus::new();
        assert_eq!(b.publish(event("first")), Ok(Some(1)));
        assert_eq!(b.publish(event("first")), Ok(None));
        assert_eq!(b.publish(event("updated")), Ok(Some(2)));
        assert_eq!(b.subscribe(Some(1)).1.len(), 1);
    }
    #[test]
    fn lag_is_explicit_and_replay_bounded() {
        let b = Bus::new();
        for n in 0..1200 {
            b.publish(event(&n.to_string())).unwrap();
        }
        let (_, replay, gap) = b.subscribe(Some(1));
        assert!(gap);
        assert!(replay.len() <= MAX_EVENTS);
    }
    #[test]
    fn oversized_event_is_rejected() {
        assert!(Bus::new().publish(event(&"x".repeat(MAX_EVENT))).is_err());
    }
}
