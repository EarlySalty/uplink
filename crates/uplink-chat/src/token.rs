//! Ausschließlich flüchtige Grants aus der zentralen OAuth-Verwaltung.
use crate::Platform;
use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use serde::Deserialize;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use zeroize::Zeroize;

#[derive(Clone, Deserialize, PartialEq, Eq)]
pub struct Grant {
    pub access_token: String,
    pub expires_at: DateTime<Utc>,
    pub platform_user_id: String,
    pub platform_login: String,
    pub scopes: Vec<String>,
}
impl std::fmt::Debug for Grant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Grant")
            .field("access_token", &"[geschützt]")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}
impl Drop for Grant {
    fn drop(&mut self) {
        self.access_token.zeroize();
    }
}
pub type Zugang = Grant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BrokerError {
    #[error("Chat-Zugang ist nicht bestätigt")]
    AccessUnconfirmed,
    #[error("Plattform ist nicht verbunden")]
    Disconnected,
    #[error("Anmeldung muss erneuert werden")]
    NeedsReauth,
    #[error("Kontoverwaltung ist nicht erreichbar")]
    Unavailable,
    #[error("Interner Zugang wurde abgewiesen")]
    Unauthorized,
}
/// Kein eigener OAuth-Flow. `refresh=true` umgeht den Broker-Cache nach einer 401.
pub trait PlatformBroker: Send + Sync {
    fn grant(
        &self,
        streamer_id: u64,
        platform: Platform,
        refresh: bool,
    ) -> BoxFuture<'_, Result<Grant, BrokerError>>;
    fn metrics(
        &self,
        _streamer_id: u64,
    ) -> BoxFuture<'_, Result<Option<crate::kennzahlen::BotKennzahlen>, BrokerError>> {
        Box::pin(async { Err(BrokerError::Unavailable) })
    }
    fn chatter_history<'a>(
        &'a self,
        _streamer_id: u64,
        _logins: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<crate::hervorhebung::VerlaufEintrag>, BrokerError>> {
        Box::pin(async { Err(BrokerError::Unavailable) })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TokenFehler {
    #[error("{0}: Chat-Zugang ist nicht bestätigt")]
    ZugangUnbestaetigt(Platform),
    #[error("{0} ist nicht verbunden")]
    NichtVerbunden(Platform),
    #[error("{0}: Anmeldung muss erneuert werden")]
    NeuAnmeldungNoetig(Platform),
    #[error("Interner Zugang wurde abgewiesen")]
    Abgelehnt,
    #[error("{0}")]
    Netz(String),
}
struct GrantRequest {
    gate: tokio::sync::Mutex<()>,
    generation: std::sync::atomic::AtomicU64,
}
type GrantKey = (i64, Platform);

/// Flüchtiger Empfänger normal bestätigter Grant-Wechsel für einen vorhandenen
/// Ressourcenbesitzer. Er fragt selbst weder Tokens an noch erneuert er sie.
pub(crate) struct GrantAbo {
    konto: String,
    scope: &'static str,
    state: Mutex<GrantAboState>,
}
struct GrantAboState {
    aktiv: bool,
    neuester: Option<Grant>,
}
impl GrantAbo {
    fn uebernehmen(&self, grant: &Grant) {
        let mut state = self.state.lock().expect("Grant-Empfänger");
        if state.aktiv
            && grant.platform_user_id == self.konto
            && grant.scopes.iter().any(|scope| scope == self.scope)
        {
            state.neuester = Some(grant.clone());
        }
    }

    pub(crate) fn stoppen(&self) {
        self.state.lock().expect("Grant-Empfänger").aktiv = false;
    }

    pub(crate) fn letzter(&self) -> Option<Grant> {
        self.state.lock().expect("Grant-Empfänger").neuester.clone()
    }

    #[cfg(test)]
    fn nehmen(&self) -> Option<Grant> {
        self.state.lock().expect("Grant-Empfänger").neuester.take()
    }
}

pub struct TokenQuelle {
    broker: Arc<dyn PlatformBroker>,
    cache: Mutex<HashMap<(i64, Platform), Grant>>,
    invalid: Mutex<std::collections::HashSet<(i64, Platform)>>,
    requests: Mutex<HashMap<GrantKey, std::sync::Weak<GrantRequest>>>,
    empfaenger: Mutex<Vec<(GrantKey, std::sync::Weak<GrantAbo>)>>,
}
impl TokenQuelle {
    pub fn from_broker(broker: Arc<dyn PlatformBroker>) -> Self {
        Self {
            broker,
            cache: Mutex::new(HashMap::new()),
            invalid: Mutex::new(Default::default()),
            requests: Mutex::new(HashMap::new()),
            empfaenger: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn grant_wechsel(
        &self,
        id: i64,
        platform: Platform,
        konto: String,
        scope: &'static str,
    ) -> Result<Arc<GrantAbo>, TokenFehler> {
        // Bindung und bereits bestätigter Anfangsstand sind gegenüber einer
        // gleichzeitig eintreffenden normalen Brokerantwort atomar.
        let cache = self.cache.lock().expect("Grants");
        let mut empfaenger = self.empfaenger.lock().expect("Grant-Empfänger");
        empfaenger.retain(|(_, weak)| weak.strong_count() > 0);
        if empfaenger.len() >= 1024 {
            return Err(TokenFehler::Netz("Kontoverwaltung ist ausgelastet".into()));
        }
        let abo = Arc::new(GrantAbo {
            konto,
            scope,
            state: Mutex::new(GrantAboState {
                aktiv: true,
                neuester: None,
            }),
        });
        if let Some(grant) = cache.get(&(id, platform)) {
            abo.uebernehmen(grant);
        }
        empfaenger.push(((id, platform), Arc::downgrade(&abo)));
        Ok(abo)
    }
    pub async fn zugang(&self, id: i64, p: Platform) -> Result<Grant, TokenFehler> {
        let uid = u64::try_from(id)
            .ok()
            .filter(|id| *id > 0)
            .ok_or(TokenFehler::Abgelehnt)?;
        let request = {
            let mut requests = self.requests.lock().expect("Grant-Anfragen");
            requests.retain(|_, v| v.strong_count() > 0);
            if let Some(request) = requests.get(&(id, p)).and_then(std::sync::Weak::upgrade) {
                request
            } else {
                if requests.len() >= 4096 {
                    return Err(TokenFehler::Netz("Kontoverwaltung ist ausgelastet".into()));
                }
                let request = Arc::new(GrantRequest {
                    gate: tokio::sync::Mutex::new(()),
                    generation: std::sync::atomic::AtomicU64::new(0),
                });
                requests.insert((id, p), Arc::downgrade(&request));
                request
            }
        };
        let _guard = request.gate.lock().await;
        let generation = request
            .generation
            .load(std::sync::atomic::Ordering::Acquire);
        if let Some(g) = self
            .cache
            .lock()
            .expect("Grants")
            .get(&(id, p))
            .filter(|g| g.expires_at > Utc::now() + chrono::Duration::seconds(60))
            .cloned()
        {
            return Ok(g);
        }
        let refresh = self.invalid.lock().expect("Grants").remove(&(id, p));
        let g = self
            .broker
            .grant(uid, p, refresh)
            .await
            .map_err(|e| match e {
                BrokerError::AccessUnconfirmed => TokenFehler::ZugangUnbestaetigt(p),
                BrokerError::Disconnected => TokenFehler::NichtVerbunden(p),
                BrokerError::NeedsReauth => TokenFehler::NeuAnmeldungNoetig(p),
                BrokerError::Unauthorized => TokenFehler::Abgelehnt,
                BrokerError::Unavailable => {
                    TokenFehler::Netz("Kontoverwaltung ist nicht erreichbar".into())
                }
            })?;
        if g.access_token.is_empty()
            || g.access_token.len() > 8192
            || g.platform_user_id.is_empty()
            || g.platform_user_id.len() > 128
            || g.platform_login.len() > 128
            || g.scopes
                .iter()
                .any(|s| s.len() > 128 || s.chars().any(char::is_control))
            || g.scopes.len() > 128
            || g.expires_at <= Utc::now()
        {
            return Err(TokenFehler::NeuAnmeldungNoetig(p));
        }
        let mut c = self.cache.lock().expect("Grants");
        if request
            .generation
            .load(std::sync::atomic::Ordering::Acquire)
            != generation
        {
            return Err(TokenFehler::Netz(
                "Zugang wurde während der Anfrage geändert".into(),
            ));
        }

        if c.len() >= 1024 {
            c.retain(|_, v| v.expires_at > Utc::now() + chrono::Duration::seconds(60));
        }
        if c.len() < 1024 {
            c.insert((id, p), g.clone());
        }
        // Erst nach Validierung und der bestehenden Generation-Prüfung. Keine
        // Benachrichtigung durch Cache-Lesen, recheck, Fehler oder alte Antworten.
        let mut empfaenger = self.empfaenger.lock().expect("Grant-Empfänger");
        empfaenger.retain(|(key, weak)| {
            let Some(abo) = weak.upgrade() else {
                return false;
            };
            if *key == (id, p) {
                abo.uebernehmen(&g);
            }
            true
        });
        Ok(g)
    }
    pub fn recheck(&self, id: i64, p: Platform) {
        if let Some(request) = self
            .requests
            .lock()
            .expect("Grant-Anfragen")
            .get(&(id, p))
            .and_then(std::sync::Weak::upgrade)
        {
            request
                .generation
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        }
        self.cache.lock().expect("Grants").remove(&(id, p));
    }
    pub fn invalidieren(&self, id: i64, p: Platform) {
        self.recheck(id, p);
        let mut i = self.invalid.lock().expect("Grants");
        if i.len() < 1024 {
            i.insert((id, p));
        }
    }
    pub fn forget(&self, id: i64) {
        // Vorher gestartete Brokerantworten dürfen nach Identitätsentzug
        // weder Cache noch Adapter wieder mit dem alten Grant versorgen.
        for platform in Platform::ALL {
            self.recheck(id, platform);
        }
        self.cache
            .lock()
            .expect("Grants")
            .retain(|(uid, _), _| *uid != id);
        self.invalid
            .lock()
            .expect("Grants")
            .retain(|(uid, _)| *uid != id);
    }
}
#[cfg(test)]
pub const PFAD: &str = "/twitch/api/v2/internal/platform-token";
#[cfg(test)]
impl TokenQuelle {
    pub fn new(base: &str, token: &str) -> Self {
        Self::from_broker(Arc::new(TestBroker {
            base: base.into(),
            token: token.into(),
        }))
    }
}
#[cfg(test)]
pub(crate) struct TestBroker {
    pub base: String,
    pub token: String,
}
#[cfg(test)]
impl PlatformBroker for TestBroker {
    fn metrics(
        &self,
        id: u64,
    ) -> BoxFuture<'_, Result<Option<crate::kennzahlen::BotKennzahlen>, BrokerError>> {
        Box::pin(async move {
            let r = reqwest::Client::new()
                .get(format!("{}{}", self.base, crate::kennzahlen::PFAD))
                .header("X-Internal-Token", &self.token)
                .query(&[("streamer", id)])
                .send()
                .await
                .map_err(|_| BrokerError::Unavailable)?;
            if r.status().as_u16() == 404 {
                return Ok(None);
            }
            if !r.status().is_success() {
                return Err(BrokerError::Unavailable);
            }
            crate::http::json(r)
                .await
                .map(Some)
                .map_err(|_| BrokerError::Unavailable)
        })
    }
    fn chatter_history<'a>(
        &'a self,
        id: u64,
        logins: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<crate::hervorhebung::VerlaufEintrag>, BrokerError>> {
        Box::pin(async move {
            let r = reqwest::Client::new()
                .get(format!("{}{}", self.base, crate::hervorhebung::PFAD))
                .header("X-Internal-Token", &self.token)
                .query(&[("streamer", id.to_string()), ("logins", logins.join(","))])
                .send()
                .await
                .map_err(|_| BrokerError::Unavailable)?;
            if r.status().as_u16() == 404 {
                return Ok(vec![]);
            }
            if !r.status().is_success() {
                return Err(BrokerError::Unavailable);
            }
            #[derive(Deserialize)]
            struct R {
                eintraege: Vec<crate::hervorhebung::VerlaufEintrag>,
            }
            crate::http::json::<R>(r)
                .await
                .map(|r| r.eintraege)
                .map_err(|_| BrokerError::Unavailable)
        })
    }

    fn grant(&self, id: u64, p: Platform, _: bool) -> BoxFuture<'_, Result<Grant, BrokerError>> {
        Box::pin(async move {
            let r = reqwest::Client::new()
                .get(format!("{}{}", self.base, PFAD))
                .header("X-Internal-Token", &self.token)
                .query(&[
                    ("streamer", id.to_string()),
                    ("platform", p.as_str().into()),
                ])
                .send()
                .await
                .map_err(|_| BrokerError::Unavailable)?;
            match r.status().as_u16() {
                200 => crate::http::json(r)
                    .await
                    .map_err(|_| BrokerError::Unavailable),
                404 => Err(BrokerError::Disconnected),
                409 => Err(BrokerError::NeedsReauth),
                401 | 403 => Err(BrokerError::Unauthorized),
                _ => Err(BrokerError::Unavailable),
            }
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grant_debug_never_contains_token() {
        let g = Grant {
            access_token: "private-fixture".into(),
            expires_at: Utc::now(),
            platform_user_id: "7".into(),
            platform_login: "example".into(),
            scopes: vec![],
        };
        assert!(!format!("{g:?}").contains("private-fixture"));
    }
    #[tokio::test]
    async fn broker_failures_stay_distinct() {
        struct B(BrokerError);
        impl PlatformBroker for B {
            fn grant(
                &self,
                _: u64,
                _: Platform,
                _: bool,
            ) -> BoxFuture<'_, Result<Grant, BrokerError>> {
                Box::pin(async { Err(self.0) })
            }
        }
        for (e, want) in [
            (
                BrokerError::Disconnected,
                TokenFehler::NichtVerbunden(Platform::Twitch),
            ),
            (
                BrokerError::NeedsReauth,
                TokenFehler::NeuAnmeldungNoetig(Platform::Twitch),
            ),
            (BrokerError::Unauthorized, TokenFehler::Abgelehnt),
            (
                BrokerError::Unavailable,
                TokenFehler::Netz("Kontoverwaltung ist nicht erreichbar".into()),
            ),
        ] {
            assert_eq!(
                TokenQuelle::from_broker(Arc::new(B(e)))
                    .zugang(7, Platform::Twitch)
                    .await
                    .unwrap_err(),
                want
            )
        }
    }
}
#[cfg(test)]
mod cache_regressions {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Counting {
        calls: AtomicUsize,
    }
    impl PlatformBroker for Counting {
        fn grant(&self, _: u64, _: Platform, _: bool) -> BoxFuture<'_, Result<Grant, BrokerError>> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::AcqRel);
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                Ok(Grant {
                    access_token: "fixture-only".into(),
                    expires_at: Utc::now() + chrono::Duration::hours(1),
                    platform_user_id: "7".into(),
                    platform_login: "example".into(),
                    scopes: vec!["user:read:chat".into()],
                })
            })
        }
    }
    #[tokio::test]
    async fn simultaneous_grant_requests_are_coalesced() {
        let broker = Arc::new(Counting {
            calls: AtomicUsize::new(0),
        });
        let source = Arc::new(TokenQuelle::from_broker(broker.clone()));
        let requests = (0..8).map(|_| {
            let source = source.clone();
            async move {
                source.zugang(7, Platform::Twitch).await.unwrap();
            }
        });
        futures::future::join_all(requests).await;
        assert_eq!(broker.calls.load(Ordering::Acquire), 1);
        source.invalidieren(7, Platform::Twitch);
        source.zugang(7, Platform::Twitch).await.unwrap();
        assert_eq!(broker.calls.load(Ordering::Acquire), 2);
    }

    #[tokio::test]
    async fn grant_changes_only_reach_the_current_matching_owner() {
        let source = TokenQuelle::from_broker(Arc::new(Counting {
            calls: AtomicUsize::new(0),
        }));
        let old = source
            .grant_wechsel(7, Platform::Twitch, "7".into(), "user:read:chat")
            .unwrap();
        let wrong_account = source
            .grant_wechsel(7, Platform::Twitch, "8".into(), "user:read:chat")
            .unwrap();
        let wrong_scope = source
            .grant_wechsel(7, Platform::Twitch, "7".into(), "events:subscribe")
            .unwrap();
        source.zugang(7, Platform::Twitch).await.unwrap();
        assert!(old.nehmen().is_some());
        assert!(wrong_account.nehmen().is_none());
        assert!(wrong_scope.nehmen().is_none());
        old.stoppen();
        let current = source
            .grant_wechsel(7, Platform::Twitch, "7".into(), "user:read:chat")
            .unwrap();
        source.recheck(7, Platform::Twitch);
        source.zugang(7, Platform::Twitch).await.unwrap();
        assert!(old.nehmen().is_none());
        assert!(current.nehmen().is_some());
        drop((old, current, wrong_account, wrong_scope));
        source.recheck(7, Platform::Twitch);
        source.zugang(7, Platform::Twitch).await.unwrap();
        assert!(source.empfaenger.lock().unwrap().is_empty());
    }

    #[test]
    fn grant_receiver_count_is_bounded_and_dead_receivers_are_removed() {
        let source = TokenQuelle::from_broker(Arc::new(Counting {
            calls: AtomicUsize::new(0),
        }));
        let receivers: Vec<_> = (0..1024)
            .map(|id| {
                source
                    .grant_wechsel(id, Platform::Kick, "7".into(), "events:subscribe")
                    .unwrap()
            })
            .collect();
        assert!(
            source
                .grant_wechsel(1025, Platform::Kick, "7".into(), "events:subscribe")
                .is_err()
        );
        drop(receivers);
        assert!(
            source
                .grant_wechsel(1025, Platform::Kick, "7".into(), "events:subscribe")
                .is_ok()
        );
        assert_eq!(source.empfaenger.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn forgotten_identity_cannot_restore_an_inflight_grant() {
        struct Paused {
            started: tokio::sync::Notify,
            finish: tokio::sync::Notify,
        }
        impl PlatformBroker for Paused {
            fn grant(
                &self,
                _: u64,
                _: Platform,
                _: bool,
            ) -> BoxFuture<'_, Result<Grant, BrokerError>> {
                Box::pin(async {
                    self.started.notify_one();
                    self.finish.notified().await;
                    Ok(Grant {
                        access_token: "synthetic".into(),
                        expires_at: Utc::now() + chrono::Duration::hours(1),
                        platform_user_id: "7".into(),
                        platform_login: "unused".into(),
                        scopes: vec!["user:read:chat".into()],
                    })
                })
            }
        }
        let broker = Arc::new(Paused {
            started: tokio::sync::Notify::new(),
            finish: tokio::sync::Notify::new(),
        });
        let source = Arc::new(TokenQuelle::from_broker(broker.clone()));
        let receiver = source
            .grant_wechsel(7, Platform::Twitch, "7".into(), "user:read:chat")
            .unwrap();
        let cloned = source.clone();
        let request = tokio::spawn(async move { cloned.zugang(7, Platform::Twitch).await });
        broker.started.notified().await;
        source.forget(7);
        broker.finish.notify_one();
        assert!(
            request.await.unwrap().is_err(),
            "Dockentzug darf keinen vorher gestarteten Grant wieder aktivieren"
        );
        assert!(receiver.nehmen().is_none());
        assert!(source.cache.lock().unwrap().is_empty());
    }
}
