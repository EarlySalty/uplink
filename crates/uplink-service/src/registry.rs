use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, Weak},
};

#[derive(Clone)]
pub struct Registry(Arc<Mutex<State>>);
struct State {
    next: u64,
    active: HashMap<u64, (u64, SessionStatus)>,
    total: usize,
    per_tenant: usize,
    recent: VecDeque<(u64, SessionStatus)>,
}
#[derive(Clone, serde::Serialize)]
pub struct SessionStatus {
    pub id: u64,
    pub state: &'static str,
    pub received_events: u64,
    pub received_bytes: u64,
    pub error: Option<&'static str>,
    pub outputs: Option<serde_json::Value>,
    pub source_observation: Option<serde_json::Value>,
}
pub struct Reservation {
    id: u64,
    tenant: u64,
    registry: Weak<Mutex<State>>,
}
impl Registry {
    pub fn new(total: usize, per_tenant: usize) -> Result<Self, &'static str> {
        if total == 0 || per_tenant == 0 || per_tenant > total {
            return Err("Sessiongrenzen sind ungültig.");
        }
        Ok(Self(Arc::new(Mutex::new(State {
            next: 1,
            active: HashMap::new(),
            total,
            per_tenant,
            recent: VecDeque::new(),
        }))))
    }
    pub fn reserve(&self, tenant: u64) -> Result<Reservation, &'static str> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| "Sessionverwaltung ist nicht verfügbar.")?;
        if tenant == 0
            || state.active.len() >= state.total
            || state.active.values().filter(|(t, _)| *t == tenant).count() >= state.per_tenant
        {
            return Err("Sessionkapazität ist belegt.");
        }
        let id = state.next;
        state.next = state
            .next
            .checked_add(1)
            .ok_or("Sessionidentitäten sind ausgeschöpft.")?;
        state.active.insert(
            id,
            (
                tenant,
                SessionStatus {
                    id,
                    state: "Eingang wird geprüft",
                    received_events: 0,
                    received_bytes: 0,
                    error: None,
                    outputs: None,
                    source_observation: None,
                },
            ),
        );
        Ok(Reservation {
            id,
            tenant,
            registry: Arc::downgrade(&self.0),
        })
    }
    pub fn active_count(&self) -> usize {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active
            .len()
    }
    pub fn status(&self, tenant: u64) -> Vec<SessionStatus> {
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let active: Vec<_> = state
            .active
            .values()
            .filter(|(t, _)| *t == tenant)
            .map(|(_, value)| value.clone())
            .collect();
        if active.is_empty() {
            state
                .recent
                .iter()
                .rev()
                .find(|(t, _)| *t == tenant)
                .map(|(_, value)| vec![value.clone()])
                .unwrap_or_default()
        } else {
            active
        }
    }
}
impl Reservation {
    pub fn fail(&self, message: &'static str) {
        self.update(|state| {
            state.error = Some(message);
            state.state = "Fehler";
        });
    }
    pub fn ended(&self) {
        self.update(|state| {
            if state.error.is_none() {
                state.state = "Beendet";
            }
        });
    }
    pub fn media_status(&self, status: serde_json::Value) {
        self.update(|state| state.outputs = Some(status));
    }
    pub fn observation(&self, observation: serde_json::Value) {
        self.update(|state| state.source_observation = Some(observation));
    }
    fn update(&self, change: impl FnOnce(&mut SessionStatus)) {
        if let Some(registry) = self.registry.upgrade() {
            let mut state = registry.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((_, status)) = state.active.get_mut(&self.id) {
                change(status);
            }
        }
    }
    pub fn id(&self) -> u64 {
        self.id
    }
    pub fn tenant(&self) -> u64 {
        self.tenant
    }
    pub fn record(&self, bytes: usize) {
        self.update(|status| {
            status.received_events = status.received_events.saturating_add(1);
            status.received_bytes = status.received_bytes.saturating_add(bytes as u64);
            status.state = "Medien werden empfangen";
        });
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            let mut state = registry.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((tenant, mut status)) = state.active.remove(&self.id) {
                if status.error.is_none() {
                    status.state = "Beendet";
                }
                state.recent.retain(|(t, _)| *t != tenant);
                state.recent.push_back((tenant, status));
                while state.recent.len() > state.total.saturating_mul(2) {
                    state.recent.pop_front();
                }
            }
        }
    }
}
