use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, Weak},
};

#[derive(Clone)]
pub struct Registry(Arc<Mutex<State>>);
struct State {
    changes: std::collections::HashSet<u64>,
    capacity: HashMap<u64, u32>,
    capacity_limit: u32,
    legacy_units: u32,
    next: u64,
    active: HashMap<u64, (u64, SessionStatus)>,
    total: usize,
    per_tenant: usize,
    recent: VecDeque<(u64, SessionStatus)>,
    output_blocks: VecDeque<(u64, String, &'static str, std::time::Instant)>,
}
#[derive(Clone, serde::Serialize)]
pub struct SessionStatus {
    pub id: u64,
    pub active: bool,
    pub generation: Option<String>,
    pub state: &'static str,
    pub received_events: u64,
    pub received_bytes: u64,
    pub error: Option<&'static str>,
    pub ingest_end_reason: Option<String>,
    pub blocked_outputs: std::collections::BTreeMap<String, &'static str>,
    pub output_notices: std::collections::BTreeMap<String, &'static str>,
    pub outputs: Option<serde_json::Value>,
    pub source_observation: Option<serde_json::Value>,
    pub frozen_layouts: serde_json::Value,
}
pub struct Reservation {
    id: u64,
    tenant: u64,
    registry: Weak<Mutex<State>>,
    report: Mutex<Option<uplink_ingest::SessionReport>>,
    ingest_ended_at: Mutex<Option<std::time::SystemTime>>,
    media_diagnostic: Mutex<Option<serde_json::Value>>,
    completion: Option<Box<dyn FnOnce(SessionCompletion) + Send + Sync>>,
}
pub struct SessionCompletion {
    pub streamer_id: u64,
    pub ended_at: std::time::SystemTime,
    pub duration: std::time::Duration,
    pub source_tracks: usize,
    pub end_reason: String,
    pub profile: serde_json::Value,
}
pub struct TenantChange {
    tenant: u64,
    registry: Weak<Mutex<State>>,
}
impl Drop for TenantChange {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            registry
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .changes
                .remove(&self.tenant);
        }
    }
}
impl Registry {
    pub fn new(total: usize, per_tenant: usize) -> Result<Self, &'static str> {
        if total == 0 || per_tenant == 0 || per_tenant > total {
            return Err("Sessiongrenzen sind ungültig.");
        }
        Ok(Self(Arc::new(Mutex::new(State {
            changes: Default::default(),
            capacity: HashMap::new(),
            capacity_limit: 0,
            legacy_units: 1,
            next: 1,
            active: HashMap::new(),
            total,
            per_tenant,
            recent: VecDeque::new(),
            output_blocks: VecDeque::new(),
        }))))
    }
    pub fn configure_capacity(&self, limit: u32, legacy_units: u32) -> Result<(), &'static str> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| "Sessionverwaltung ist nicht verfügbar.")?;
        if !state.active.is_empty() || legacy_units == 0 || (limit > 0 && legacy_units > limit) {
            return Err("Kapazitätskonfiguration kann nicht übernommen werden.");
        }
        state.capacity_limit = limit;
        state.legacy_units = legacy_units;
        Ok(())
    }
    pub fn reserve(&self, tenant: u64) -> Result<Reservation, &'static str> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| "Sessionverwaltung ist nicht verfügbar.")?;
        if tenant == 0
            || state.changes.contains(&tenant)
            || state.active.len() >= state.total
            || state.active.values().filter(|(t, _)| *t == tenant).count() >= state.per_tenant
        {
            return Err("Sessionkapazität ist belegt.");
        }
        let used: u64 = state.capacity.values().map(|value| u64::from(*value)).sum();
        if state.capacity_limit > 0
            && used + u64::from(state.legacy_units) > u64::from(state.capacity_limit)
        {
            return Err("Gemessene Medienkapazität ist belegt.");
        }
        let id = state.next;
        state.next = state
            .next
            .checked_add(1)
            .ok_or("Sessionidentitäten sind ausgeschöpft.")?;
        let legacy_units = state.legacy_units;
        state.capacity.insert(id, legacy_units);
        state.active.insert(
            id,
            (
                tenant,
                SessionStatus {
                    id,
                    active: true,
                    generation: None,
                    state: "Eingang wird geprüft",
                    received_events: 0,
                    received_bytes: 0,
                    error: None,
                    ingest_end_reason: None,
                    blocked_outputs: std::collections::BTreeMap::new(),
                    output_notices: std::collections::BTreeMap::new(),
                    outputs: None,
                    source_observation: None,
                    frozen_layouts: serde_json::Value::Null,
                },
            ),
        );
        Ok(Reservation {
            id,
            tenant,
            registry: Arc::downgrade(&self.0),
            report: Mutex::new(None),
            ingest_ended_at: Mutex::new(None),
            media_diagnostic: Mutex::new(None),
            completion: None,
        })
    }
    pub fn begin_change(&self, tenant: u64) -> Result<Arc<TenantChange>, &'static str> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| "Sessionverwaltung ist nicht verfügbar.")?;
        if tenant == 0
            || state.changes.contains(&tenant)
            || state.active.values().any(|(id, _)| *id == tenant)
        {
            return Err(
                "Ein Stream oder eine Zieländerung läuft. Stream zuerst beenden und erneut versuchen.",
            );
        }
        state.changes.insert(tenant);
        Ok(Arc::new(TenantChange {
            tenant,
            registry: Arc::downgrade(&self.0),
        }))
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
    pub fn reserve_profile_capacity(&self, units: u32) -> Result<(), &'static str> {
        let registry = self.registry.upgrade().ok_or("Session ist beendet.")?;
        let mut state = registry
            .lock()
            .map_err(|_| "Sessionverwaltung ist nicht verfügbar.")?;
        if units == 0 || state.capacity_limit == 0 {
            return Err(
                "Dieses Qualitätsprofil ist noch nicht durch eine Lastmessung freigegeben.",
            );
        }
        let current = *state.capacity.get(&self.id).ok_or("Session ist beendet.")?;
        let requested = current
            .checked_add(units)
            .ok_or("Gemessene Medienkapazität ist belegt.")?;
        let used: u64 = state.capacity.values().map(|value| u64::from(*value)).sum();
        if used + u64::from(units) > u64::from(state.capacity_limit) {
            return Err("Für dieses Qualitätsprofil ist die gemessene Medienkapazität belegt.");
        }
        state.capacity.insert(self.id, requested);
        Ok(())
    }

    pub fn media_diagnostic(&self, value: serde_json::Value) {
        *self
            .media_diagnostic
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(value);
    }
    pub fn on_completion(
        &mut self,
        completion: impl FnOnce(SessionCompletion) + Send + Sync + 'static,
    ) {
        self.completion = Some(Box::new(completion));
    }
    pub fn ingest_report(&self, report: &uplink_ingest::SessionReport) {
        *self
            .ingest_ended_at
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(std::time::SystemTime::now());
        self.generation(report.generation);
        self.ingest_ended(&report.reason);
        *self
            .report
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(report.clone());
    }
    pub fn generation(&self, generation: uplink_ingest::ConnectionGeneration) {
        // Nur servergenerierte Zufallsinstanz und Zähler, keine Zugangsdaten.
        self.update(|state| state.generation = Some(format!("{generation:?}")));
    }
    pub fn ingest_ended(&self, reason: &uplink_ingest::EndReason) {
        self.update(|state| {
            // EndReason/MediaError enthalten ausschließlich geprüfte Enumwerte,
            // keine fremden Protokolltexte, Adressen oder Zugangsdaten.
            state.ingest_end_reason=Some(format!("{reason:?}"));
            if !matches!(reason,uplink_ingest::EndReason::ExplicitStop | uplink_ingest::EndReason::PeerClosed) && state.error.is_none() {
                state.error=Some("Eingang wurde unterbrochen oder abgewiesen; der Endgrund ist im Status verfügbar.");
                state.state="Fehler";
            }
        });
    }
    pub fn block_output(&self, platform: String, message: &'static str) {
        if self.record_output_block(&platform, message, std::time::Instant::now()) {
            let platform = match platform.as_str() {
                "twitch" | "youtube" | "kick" | "tiktok" => platform.as_str(),
                _ => "unbekannt",
            };
            eprintln!(
                "Uplink-Ausgabe blockiert: streamer_id={} platform={} Grund={}",
                self.tenant, platform, message
            );
        }
    }
    fn record_output_block(
        &self,
        platform: &str,
        message: &'static str,
        now: std::time::Instant,
    ) -> bool {
        let Some(registry) = self.registry.upgrade() else {
            return false;
        };
        let mut state = registry.lock().unwrap_or_else(|error| error.into_inner());
        let Some((_, status)) = state.active.get_mut(&self.id) else {
            return false;
        };
        status.blocked_outputs.insert(platform.to_owned(), message);
        state.output_blocks.retain(|(_, _, _, seen)| {
            now.saturating_duration_since(*seen) < std::time::Duration::from_secs(300)
        });
        let repeated = state
            .output_blocks
            .iter()
            .any(|(tenant, target, reason, _)| {
                *tenant == self.tenant && target == platform && *reason == message
            });
        state
            .output_blocks
            .retain(|(tenant, target, _, _)| *tenant != self.tenant || target != platform);
        state
            .output_blocks
            .push_back((self.tenant, platform.to_owned(), message, now));
        while state.output_blocks.len() > 1024 {
            state.output_blocks.pop_front();
        }
        !repeated
    }
    pub fn output_notice(&self, platform: String, message: &'static str) {
        self.update(|state| {
            state.output_notices.insert(platform, message);
        });
    }
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
    pub fn freeze_layout(&self, platform: &str, layout_id: u64, revision: u64) {
        self.update(|state| {
            let eintrag = serde_json::json!({"layout_id":layout_id,"revision":revision});
            match &mut state.frozen_layouts {
                serde_json::Value::Null => {
                    state.frozen_layouts = serde_json::json!({platform: eintrag});
                }
                map @ serde_json::Value::Object(_) => {
                    map[platform] = eintrag;
                }
                _ => {}
            }
        });
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
            if status.error.is_none() {
                status.state = "Medien werden empfangen";
            }
        });
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        let Some(registry) = self.registry.upgrade() else {
            return;
        };
        let snapshot = registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active
            .get(&self.id)
            .cloned();
        if let Some((tenant, mut status)) = snapshot {
            status.active = false;
            if status.error.is_none() {
                status.state = "Beendet";
            }
            let completed = self.completion.take().map(|completion| {
                    let report = self
                        .report
                        .get_mut()
                        .unwrap_or_else(|error| error.into_inner());
                    let reason = report
                        .as_ref()
                        .map_or(uplink_ingest::EndReason::TaskFailed, |report| report.reason);
                    let mut end_reason = match (reason, status.error) {
                        (uplink_ingest::EndReason::ConsumerClosed, Some(error)) => {
                            format!("ConsumerClosed: {error}")
                        }
                        _ => format!("{reason:?}"),
                    };
                    status.ingest_end_reason = Some(end_reason.clone());
                    let media_diagnostic = self
                        .media_diagnostic
                        .get_mut()
                        .unwrap_or_else(|error| error.into_inner())
                        .take();
                    if let Some(diagnostic) = &media_diagnostic {
                        end_reason.push_str("; Diagnose=");
                        end_reason.push_str(&diagnostic.to_string());
                    }
                    (completion, SessionCompletion {
                        streamer_id: tenant,
                        ended_at: self
                            .ingest_ended_at
                            .get_mut()
                            .unwrap_or_else(|error| error.into_inner())
                            .unwrap_or_else(std::time::SystemTime::now),
                        duration: report
                            .as_ref()
                            .map_or(std::time::Duration::ZERO, |report| report.duration),
                        source_tracks: report.as_ref().map_or(0, |report| report.track_count),
                        end_reason,
                        profile: serde_json::json!({
                            "media_diagnostic": media_diagnostic,
                            "source_observation": status.source_observation,
                            "outputs": status.outputs,
                            "received_events": report.as_ref().map_or(0, |report| report.received_events),
                            "received_bytes": report.as_ref().map_or(0, |report| report.received_bytes),
                            "source_tracks": report.as_ref().map_or(0, |report| report.track_count),
                        }),
                    })
                });
            let mut state = registry.lock().unwrap_or_else(|e| e.into_inner());
            state.active.remove(&self.id);
            state.capacity.remove(&self.id);
            state.recent.retain(|(t, _)| *t != tenant);
            state.recent.push_back((tenant, status));
            while state.recent.len() > state.total.saturating_mul(2) {
                state.recent.pop_front();
            }
            drop(state);
            if let Some((completion, record)) = completed {
                completion(record);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_block_journal_is_debounced_across_reconnects_and_bounded() {
        let registry = Registry::new(2, 1).unwrap();
        let now = std::time::Instant::now();
        for attempt in 0..12 {
            let reservation = registry.reserve(538636411).unwrap();
            assert_eq!(
                reservation.record_output_block("twitch", "Profil fehlt.", now),
                attempt == 0
            );
            assert_eq!(
                registry.status(538636411)[0].blocked_outputs["twitch"],
                "Profil fehlt."
            );
        }
        let reservation = registry.reserve(538636411).unwrap();
        assert!(reservation.record_output_block("twitch", "Kapazität fehlt.", now));
        assert!(reservation.record_output_block("youtube", "Kapazität fehlt.", now));
        assert!(reservation.record_output_block(
            "twitch",
            "Kapazität fehlt.",
            now + std::time::Duration::from_secs(301)
        ));
        drop(reservation);
        for tenant in 1..=1100 {
            let reservation = registry.reserve(tenant).unwrap();
            assert!(reservation.record_output_block("twitch", "Profil fehlt.", now));
        }
        assert_eq!(registry.0.lock().unwrap().output_blocks.len(), 1024);
    }

    #[test]
    fn media_updates_preserve_unmeasured_output_notice() {
        let registry = Registry::new(1, 1).unwrap();
        let reservation = registry.reserve(11).unwrap();
        reservation.output_notice("twitch".into(), "Leiter ist nicht lastgemessen.");
        reservation.media_status(serde_json::json!({"outputs":[]}));
        assert_eq!(
            registry.status(11)[0].output_notices["twitch"],
            "Leiter ist nicht lastgemessen."
        );
    }

    #[test]
    fn eingefrorene_layoutrevision_erscheint_je_plattform_im_status() {
        let registry = Registry::new(2, 1).unwrap();
        let reservation = registry.reserve(31).unwrap();
        reservation.freeze_layout("twitch", 7, 3);
        let status = registry.status(31);
        assert_eq!(
            status.first().unwrap().frozen_layouts["twitch"]["revision"],
            3
        );
        assert_eq!(
            status.first().unwrap().frozen_layouts["twitch"]["layout_id"],
            7
        );
        reservation.freeze_layout("twitch", 8, 4);
        let status = registry.status(31);
        assert_eq!(
            status.first().unwrap().frozen_layouts["twitch"]["revision"],
            4
        );
    }
}
