use crate::{
    BrokerError, Platform, PlatformBroker,
    adapter::{AdapterFabrik, ChatAdapter, ChatFehler, PlattformFabrik},
    bus::Bus,
    kick_webhook::KickDrehkreuz,
    token::TokenQuelle,
};
use futures::{
    future::BoxFuture,
    stream::{FuturesUnordered, StreamExt},
};
use serde::Serialize;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{Semaphore, mpsc, watch};
use tokio_util::sync::CancellationToken;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DockUser {
    pub streamer_id: u64,
    pub enabled: bool,
}
pub trait DockIdentity: Send + Sync {
    fn resolve(&self, sha256: [u8; 32]) -> BoxFuture<'_, Result<Option<DockUser>, BrokerError>>;
}
#[derive(Clone)]
pub struct ChatConfig {
    pub allowed_origins: Vec<String>,
    pub max_users: usize,
    pub max_sockets_per_user: usize,
    pub idle_timeout: Duration,
}
impl Default for ChatConfig {
    fn default() -> Self {
        Self {
            allowed_origins: vec!["https://deutsche-deadlock-community.de".into()],
            max_users: 128,
            max_sockets_per_user: 8,
            idle_timeout: Duration::from_secs(120),
        }
    }
}
pub struct ChatHub {
    pub(crate) config: ChatConfig,
    pub(crate) identity: Arc<dyn DockIdentity>,
    pub(crate) users: Mutex<HashMap<u64, Arc<User>>>,
    pub(crate) kick: Arc<KickDrehkreuz>,
    pub(crate) info: Arc<crate::streaminfo::StreamInfoDienst>,
    pub(crate) points: Arc<dyn crate::punkte::PunkteDienst>,
    factory: Arc<dyn AdapterFabrik>,
    source: Arc<TokenQuelle>,
    broker: Arc<dyn PlatformBroker>,
    cancel: CancellationToken,
    pub(crate) requests: Semaphore,
}
pub(crate) struct User {
    pub id: u64,
    pub generation: String,
    pub access: watch::Sender<bool>,
    pub bus: Bus,
    pub adapters: Mutex<HashMap<Platform, Arc<dyn ChatAdapter>>>,
    pub errors: Mutex<HashMap<Platform, ChatFehler>>,
    pub cancel: CancellationToken,
    pub socket_cancel: Mutex<CancellationToken>,
    pub sockets: AtomicUsize,
    pub active: AtomicBool,
    session_epoch: AtomicU64,
    pub last_seen: Mutex<Instant>,
    pub actions: Arc<Semaphore>,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    pub metrics: Mutex<Option<crate::kennzahlen::Kennzahlen>>,
}
/// Eine Berechtigungsabfrage gilt ausschließlich für diese Hubinstanz.
#[derive(Clone)]
pub struct IdentityCheck {
    pub streamer_id: u64,
    pub generation: String,
    pub active: bool,
}
impl User {
    pub fn access_confirmed(&self) -> bool {
        *self.access.borrow()
    }
    pub fn resume(
        &self,
        generation: Option<&str>,
        since: Option<u64>,
    ) -> (
        tokio::sync::broadcast::Receiver<crate::nachricht::Rahmen>,
        Vec<crate::nachricht::Rahmen>,
        bool,
    ) {
        let matches = generation == Some(self.generation.as_str());
        let (receiver, replay, gap) = self.bus.subscribe(if matches { since } else { None });
        (receiver, replay, gap || (generation.is_some() && !matches))
    }
}
impl Drop for User {
    fn drop(&mut self) {
        self.cancel.cancel();
        for task in self.tasks.get_mut().expect("Tasks").drain(..) {
            task.abort();
        }
    }
}
impl Drop for ChatHub {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
#[derive(Serialize)]
pub struct Status {
    pub platform: Platform,
    pub eingerichtet: bool,
    pub verbunden: bool,
    pub hinweis: Option<String>,
    pub fehlende_scopes: Vec<String>,
    pub zustand: &'static str,
}
impl ChatHub {
    /// Nur bestehende Dockverbindungen schließen. Kanaladapter, aktive Session
    /// und Kennzahlen gehören zur Plattformidentität und bleiben erhalten.
    pub fn rotate_docks(&self, id: u64) {
        if let Some(user) = self.users.lock().expect("Nutzer").get(&id) {
            let mut sockets = user.socket_cancel.lock().expect("Dockverbindungen");
            sockets.cancel();
            *sockets = user.cancel.child_token();
        }
    }
    /// Identität wurde vom Dienst bereits dauerhaft und mandantenbezogen geprüft.
    pub fn status_for(self: &Arc<Self>, id: u64) -> Result<Vec<Status>, &'static str> {
        let user = self.ensure(id)?;
        Ok(self.status(&user))
    }
    pub fn user_ids(&self) -> Vec<u64> {
        self.users.lock().expect("Nutzer").keys().copied().collect()
    }
    pub fn identity_checks(&self) -> Vec<IdentityCheck> {
        self.users
            .lock()
            .expect("Nutzer")
            .values()
            .map(|user| IdentityCheck {
                streamer_id: user.id,
                generation: user.generation.clone(),
                active: user.active.load(Ordering::Acquire),
            })
            .collect()
    }
    /// None ist ein unbekanntes Abfrageergebnis, Some(false) eine bestätigte Sperre.
    pub fn identity_checked(&self, check: &IdentityCheck, allowed: Option<bool>) {
        let mut users = self.users.lock().expect("Nutzer");
        let Some(user) = users
            .get(&check.streamer_id)
            .filter(|user| user.generation == check.generation)
        else {
            return;
        };
        if allowed == Some(false) {
            let user = users.remove(&check.streamer_id).expect("Geprüfter Nutzer");
            user.cancel.cancel();
            self.source.forget(check.streamer_id as i64);
            return;
        }
        let allowed = allowed == Some(true);
        if user.access_confirmed() != allowed {
            let mut sockets = user.socket_cancel.lock().expect("Dockverbindungen");
            sockets.cancel();
            if allowed {
                *sockets = user.cancel.child_token();
            }
            user.access.send_replace(allowed);
        }
    }
    /// Nach Nutzersperre bestehende Adapter und
    /// Websockets abbrechen; neue Zugriffe müssen die neue Identität prüfen.
    pub fn invalidate(&self, id: u64) {
        if let Some(user) = self.users.lock().expect("Nutzer").remove(&id) {
            user.cancel.cancel();
        }
        self.source.forget(id as i64);
    }
    pub fn new(
        config: ChatConfig,
        identity: Arc<dyn DockIdentity>,
        broker: Arc<dyn PlatformBroker>,
    ) -> Result<Arc<Self>, &'static str> {
        if config.max_users == 0
            || config.max_users > 1024
            || config.max_sockets_per_user == 0
            || config.max_sockets_per_user > 32
            || config.idle_timeout < Duration::from_secs(30)
            || config.allowed_origins.is_empty()
        {
            return Err("Chatkonfiguration ist ungültig");
        }
        for origin in &config.allowed_origins {
            let u = reqwest::Url::parse(origin).map_err(|_| "Dock-Origin ist ungültig")?;
            if !matches!(u.scheme(), "https" | "http")
                || u.origin().ascii_serialization() != *origin
            {
                return Err("Dock-Origin ist ungültig");
            }
        }
        let source = Arc::new(TokenQuelle::from_broker(broker.clone()));
        let kick = Arc::new(KickDrehkreuz::new(crate::kick_webhook::KICK_PUBLIC_KEY_URL));
        let helix = Arc::new(crate::helix::HelixFabrik::new(source.clone()));
        let factory = Arc::new(PlattformFabrik::neu(source.clone(), kick.clone()));
        Ok(Arc::new(Self {
            config,
            identity,
            users: Mutex::new(HashMap::new()),
            kick,
            info: crate::streaminfo::StreamInfoDienst::new(Arc::new(
                crate::streaminfo::twitch::TwitchStreamInfoFabrik::new(helix.clone()),
            )),
            points: Arc::new(crate::punkte::TwitchPunkte::new(helix)),
            factory,
            source,
            broker,
            cancel: CancellationToken::new(),
            requests: Semaphore::new(64),
        }))
    }
    pub(crate) fn ensure(self: &Arc<Self>, id: u64) -> Result<Arc<User>, &'static str> {
        if id == 0 || id > i64::MAX as u64 || self.cancel.is_cancelled() {
            return Err("Chat ist nicht verfügbar");
        }
        let mut users = self.users.lock().expect("Nutzer");
        users.retain(|_, u| !u.cancel.is_cancelled());
        if let Some(u) = users.get(&id) {
            *u.last_seen.lock().expect("Aktivität") = Instant::now();
            return Ok(u.clone());
        }
        if users.len() >= self.config.max_users {
            return Err("Chat ist ausgelastet");
        }
        let cancel = self.cancel.child_token();
        use ring::rand::SecureRandom;
        let mut nonce = [0_u8; 16];
        ring::rand::SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| "Chatgeneration konnte nicht erzeugt werden")?;
        let generation = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        let u = Arc::new(User {
            id,
            generation,
            access: watch::channel(true).0,
            bus: Bus::new(),
            adapters: Mutex::new(HashMap::new()),
            errors: Mutex::new(HashMap::new()),
            socket_cancel: Mutex::new(cancel.child_token()),
            cancel,
            sockets: AtomicUsize::new(0),
            active: AtomicBool::new(false),
            session_epoch: AtomicU64::new(0),
            last_seen: Mutex::new(Instant::now()),
            actions: Arc::new(Semaphore::new(1)),
            tasks: Mutex::new(vec![]),
            metrics: Mutex::new(None),
        });
        users.insert(id, u.clone());
        let weak = Arc::downgrade(self);
        let user = Arc::downgrade(&u);
        let (t, mut events) = mpsc::channel(256);
        let cancel = u.cancel.clone();
        let idle = self.config.idle_timeout;
        let collector = {
            let user = user.clone();
            let broker = self.broker.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move {
                let highlighter = crate::hervorhebung::Hervorheber::new(Some(Arc::new(
                    crate::hervorhebung::ChatVerlaufQuelle::from_broker(broker.clone()),
                )));
                let counts = crate::kennzahlen::SessionZaehler::new();
                let mut session_epoch = 0;
                let source = crate::kennzahlen::KennzahlenQuelle::from_broker(broker);
                let mut tick = tokio::time::interval(Duration::from_secs(30));
                loop {
                    tokio::select! {biased;_=cancel.cancelled()=>break,event=events.recv()=>{let Some(mut event)=event else{break};let Some(user)=user.upgrade()else{break};
                      if !user.access_confirmed(){continue}
                      sync_session(&user,&mut session_epoch,&counts,&highlighter);
                      if user.bus.is_duplicate(&event){continue}
                      let _=tokio::time::timeout(Duration::from_secs(2),highlighter.anreichern(id as i64,&mut event)).await;
                      match user.bus.publish(event.clone()){Ok(Some(_))=>counts.zaehlen(id as i64,&event),Ok(None)=>{},Err(_)=>{user.cancel.cancel();break}}
                    },_=tick.tick()=>{let Some(user)=user.upgrade()else{break};if user.sockets.load(Ordering::Relaxed)==0&&!user.active.load(Ordering::Relaxed)&&user.last_seen.lock().expect("Aktivität").elapsed()>idle{user.cancel.cancel();break}
                      if !user.access_confirmed(){continue}
                      sync_session(&user,&mut session_epoch,&counts,&highlighter);
                      let(bot,at)=tokio::time::timeout(Duration::from_secs(3),source.stand(id as i64)).await.unwrap_or((None,None));
                      let frame=crate::kennzahlen::rahmen_bauen(counts.seit(id as i64).unwrap_or_else(chrono::Utc::now),user.active.load(Ordering::Relaxed),counts.top(id as i64,3),&counts.anzeigenamen(id as i64),bot.as_ref(),at);*user.metrics.lock().expect("Kennzahlen")=Some(frame);
                    }}
                }
            })
        };
        let mut access = u.access.subscribe();
        let driver = tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(30));
            loop {
                tokio::select! {biased;_=cancel.cancelled()=>break,_=access.changed()=>{},_=tick.tick()=>{}}
                let Some(hub) = weak.upgrade() else { break };
                let Some(user) = user.upgrade() else { break };
                if !user.access_confirmed() {
                    let adapters: Vec<_> = user
                        .adapters
                        .lock()
                        .expect("Adapter")
                        .drain()
                        .map(|(_, adapter)| adapter)
                        .collect();
                    futures::future::join_all(adapters.iter().map(|adapter| adapter.trennen()))
                        .await;
                    continue;
                }
                let mut starts = FuturesUnordered::new();
                for p in Platform::ALL {
                    let h = hub.clone();
                    let u = user.clone();
                    let t = t.clone();
                    starts.push(async move {
                        h.reconcile(&u, p, t).await;
                    });
                }
                tokio::select! {biased;_=cancel.cancelled()=>break,_=access.changed()=>{tick.reset_immediately()},_=async{while starts.next().await.is_some(){}}=>{}}
            }
            if let Some(user) = user.upgrade() {
                let adapters: Vec<_> = user
                    .adapters
                    .lock()
                    .expect("Adapter")
                    .drain()
                    .map(|(_, a)| a)
                    .collect();
                futures::future::join_all(adapters.iter().map(|a| a.trennen())).await;
            }
            if let Some(h) = weak.upgrade() {
                h.source.forget(id as i64);
            }
        });
        u.tasks.lock().expect("Tasks").extend([collector, driver]);
        Ok(u)
    }
    async fn reconcile(
        &self,
        user: &Arc<User>,
        p: Platform,
        input: mpsc::Sender<crate::nachricht::Ereignis>,
    ) {
        if !user.access_confirmed() {
            return;
        }
        if p == Platform::TikTok {
            user.errors
                .lock()
                .expect("Fehler")
                .insert(p, ChatFehler::NichtUnterstuetzt(p));
            return;
        }
        self.source.recheck(user.id as i64, p);
        let grant = tokio::time::timeout(
            Duration::from_secs(5),
            self.source.zugang(user.id as i64, p),
        )
        .await;
        let account = grant
            .as_ref()
            .ok()
            .and_then(|r| r.as_ref().ok())
            .map(|g| g.platform_user_id.clone());
        let error = match grant {
            Ok(Ok(_)) => None,
            Ok(Err(e)) => Some(crate::helix::fehler_aus_token(e)),
            Err(_) => Some(ChatFehler::Netz(
                "Kontoverwaltung ist nicht erreichbar".into(),
            )),
        };
        if let Some(e) = error {
            let old = user.adapters.lock().expect("Adapter").remove(&p);
            if let Some(a) = old {
                a.trennen().await
            }
            user.errors.lock().expect("Fehler").insert(p, e);
            return;
        }
        let existing = user.adapters.lock().expect("Adapter").get(&p).cloned();
        if let Some(a) = existing {
            if a.ende_grund().is_none() && a.account_id() == account.as_deref() {
                return;
            }
            a.trennen().await;
            user.adapters.lock().expect("Adapter").remove(&p);
        }
        let result = tokio::time::timeout(Duration::from_secs(15), async {
            let a = self.factory.bauen(user.id as i64, p, input).await?;
            a.verbinden().await?;
            Ok::<_, ChatFehler>(a)
        })
        .await
        .unwrap_or_else(|_| {
            Err(ChatFehler::Netz(
                "Chatverbindung hat die Frist überschritten".into(),
            ))
        });
        match result {
            Ok(a) => {
                user.adapters.lock().expect("Adapter").insert(p, a);
                user.errors.lock().expect("Fehler").remove(&p);
            }
            Err(e) => {
                user.errors.lock().expect("Fehler").insert(p, e);
            }
        }
    }
    /// Der Dienst meldet echte Streamstarts und -enden; geöffnete Docks zählen nicht als Livestream.
    pub fn set_active(self: &Arc<Self>, id: u64, active: bool) -> Result<(), &'static str> {
        let user = self.ensure(id)?;
        if user.active.swap(active, Ordering::AcqRel) != active {
            user.session_epoch.fetch_add(1, Ordering::AcqRel);
        }
        Ok(())
    }
    pub async fn shutdown(&self) {
        self.cancel.cancel();
        let users: Vec<_> = self
            .users
            .lock()
            .expect("Nutzer")
            .drain()
            .map(|(_, u)| u)
            .collect();
        let mut tasks = Vec::new();
        for u in &users {
            tasks.extend(std::mem::take(&mut *u.tasks.lock().expect("Tasks")));
        }
        if tokio::time::timeout(Duration::from_secs(12), async {
            for task in &mut tasks {
                let _ = task.await;
            }
        })
        .await
        .is_err()
        {
            for task in &tasks {
                task.abort();
            }
            for task in tasks {
                let _ = task.await;
            }
        }
    }
    pub(crate) fn status(&self, u: &User) -> Vec<Status> {
        if !u.access_confirmed() {
            return Platform::ALL.into_iter().map(|platform| Status {platform,eingerichtet:false,verbunden:false,hinweis:Some("Chatzugang konnte gerade nicht geprüft werden. Verbindung pausiert; Wiederanlauf erfolgt automatisch.".into()),fehlende_scopes:vec![],zustand:"access_unconfirmed"}).collect();
        }
        let adapters = u.adapters.lock().expect("Adapter");
        let errors = u.errors.lock().expect("Fehler");
        Platform::ALL
            .into_iter()
            .map(|p| {
                let a = adapters.get(&p);
                let e = a
                    .and_then(|a| a.ende_grund())
                    .or_else(|| errors.get(&p).cloned());
                Status {
                    platform: p,
                    zustand: match &e {
                        Some(ChatFehler::ZugangUnbestaetigt(_)) => "access_unconfirmed",
                        Some(ChatFehler::NichtVerbunden(_)) => "disconnected",
                        Some(ChatFehler::NeuAnmeldungNoetig(_)) => "needs_reauth",
                        Some(ChatFehler::NichtUnterstuetzt(_)) => "unsupported",
                        Some(_) => "unavailable",
                        None if a.is_some_and(|a| a.verbunden()) => "connected",
                        None => "connecting",
                    },
                    eingerichtet: a.is_some()
                        || matches!(e, Some(ChatFehler::NeuAnmeldungNoetig(_))),
                    verbunden: a.is_some_and(|a| a.verbunden()),
                    hinweis: e.as_ref().map(crate::supervisor::hinweis_fuer),
                    fehlende_scopes: a.map(|a| a.fehlende_scopes()).unwrap_or_default(),
                }
            })
            .collect()
    }
}

fn sync_session(
    user: &User,
    seen: &mut u64,
    counts: &crate::kennzahlen::SessionZaehler,
    highlighter: &crate::hervorhebung::Hervorheber,
) {
    let epoch = user.session_epoch.load(Ordering::Acquire);
    if epoch != *seen {
        *seen = epoch;
        counts.beenden(user.id as i64);
        highlighter.beenden(user.id as i64);
        if user.active.load(Ordering::Acquire) {
            counts.starten(user.id as i64);
            highlighter.starten(user.id as i64);
        }
    }
}

#[cfg(test)]
mod rotation_tests {
    use super::*;
    struct Offline;
    impl DockIdentity for Offline {
        fn resolve(&self, _: [u8; 32]) -> BoxFuture<'_, Result<Option<DockUser>, BrokerError>> {
            Box::pin(async { Ok(None) })
        }
    }
    impl PlatformBroker for Offline {
        fn grant(
            &self,
            _: u64,
            _: Platform,
            _: bool,
        ) -> BoxFuture<'_, Result<crate::Grant, BrokerError>> {
            Box::pin(async { Err(BrokerError::Disconnected) })
        }
    }
    #[tokio::test]
    async fn dock_rotation_keeps_active_session_and_cached_metrics() {
        let hub =
            ChatHub::new(ChatConfig::default(), Arc::new(Offline), Arc::new(Offline)).unwrap();
        hub.set_active(7, true).unwrap();
        let user = hub.ensure(7).unwrap();
        let epoch = user.session_epoch.load(Ordering::Acquire);
        let socket = user.socket_cancel.lock().unwrap().clone();
        let frame = crate::kennzahlen::rahmen_bauen(
            chrono::Utc::now(),
            true,
            vec![],
            &HashMap::new(),
            None,
            None,
        );
        *user.metrics.lock().unwrap() = Some(frame.clone());
        let info = crate::nachricht::Ereignis::Info(
            serde_json::from_value(
                serde_json::json!({"platform":"twitch","channel_id":"7","title":"Aktiv"}),
            )
            .unwrap(),
        );
        user.bus.publish(info).unwrap();
        hub.rotate_docks(7);
        assert!(socket.is_cancelled());
        let current = hub.ensure(7).unwrap();
        assert!(Arc::ptr_eq(&user, &current));
        assert!(!current.cancel.is_cancelled());
        assert!(!current.socket_cancel.lock().unwrap().is_cancelled());
        assert!(current.active.load(Ordering::Acquire));
        assert_eq!(current.session_epoch.load(Ordering::Acquire), epoch);
        assert_eq!(*current.metrics.lock().unwrap(), Some(frame));
        assert_eq!(current.bus.subscribe(None).1.len(), 1);
        hub.invalidate(7);
        assert!(current.cancel.is_cancelled());
        assert!(current.socket_cancel.lock().unwrap().is_cancelled());
        assert!(hub.user_ids().is_empty());
        hub.shutdown().await;
    }
    #[tokio::test]
    async fn unknown_identity_pauses_but_preserves_the_active_generation() {
        let hub =
            ChatHub::new(ChatConfig::default(), Arc::new(Offline), Arc::new(Offline)).unwrap();
        hub.set_active(7, true).unwrap();
        let user = hub.ensure(7).unwrap();
        let epoch = user.session_epoch.load(Ordering::Acquire);
        let check = hub.identity_checks().pop().unwrap();
        let old_socket = user.socket_cancel.lock().unwrap().clone();
        let frame = crate::kennzahlen::rahmen_bauen(
            chrono::Utc::now(),
            true,
            vec![],
            &HashMap::new(),
            None,
            None,
        );
        *user.metrics.lock().unwrap() = Some(frame.clone());
        hub.identity_checked(&check, None);
        assert!(old_socket.is_cancelled());
        assert!(!user.access_confirmed());
        assert!(user.active.load(Ordering::Acquire));
        assert!(Arc::ptr_eq(&user, &hub.ensure(7).unwrap()));
        assert_eq!(*user.metrics.lock().unwrap(), Some(frame.clone()));
        assert!(
            hub.status_for(7)
                .unwrap()
                .iter()
                .all(|status| !status.verbunden && status.zustand == "access_unconfirmed")
        );
        hub.identity_checked(&check, Some(true));
        assert!(user.access_confirmed());
        assert!(!user.socket_cancel.lock().unwrap().is_cancelled());
        assert_eq!(user.session_epoch.load(Ordering::Acquire), epoch);
        assert_eq!(*user.metrics.lock().unwrap(), Some(frame));
        hub.identity_checked(&check, Some(false));
        assert!(user.cancel.is_cancelled());
        let replacement = hub.ensure(7).unwrap();
        hub.identity_checked(&check, Some(false));
        hub.identity_checked(&check, None);
        assert!(replacement.access_confirmed());
        assert!(!replacement.cancel.is_cancelled());
        assert_ne!(replacement.generation, check.generation);
        hub.shutdown().await;
    }
}

#[cfg(test)]
mod independent_review_probes {
    use super::*;
    struct Offline;
    impl DockIdentity for Offline {
        fn resolve(&self, _: [u8; 32]) -> BoxFuture<'_, Result<Option<DockUser>, BrokerError>> {
            Box::pin(async { Ok(None) })
        }
    }
    impl PlatformBroker for Offline {
        fn grant(
            &self,
            _: u64,
            _: Platform,
            _: bool,
        ) -> BoxFuture<'_, Result<crate::Grant, BrokerError>> {
            Box::pin(async { Err(BrokerError::Disconnected) })
        }
    }
    #[tokio::test]
    async fn review_recreated_user_must_not_accept_previous_bus_cursor() {
        let hub =
            ChatHub::new(ChatConfig::default(), Arc::new(Offline), Arc::new(Offline)).unwrap();
        let previous = hub.ensure(7).unwrap();
        let previous_generation = previous.generation.clone();
        let event = |title| {
            crate::nachricht::Ereignis::Info(
                serde_json::from_value(
                    serde_json::json!({"platform":"twitch","channel_id":"7","title":title}),
                )
                .unwrap(),
            )
        };
        for title in ["old A", "old B", "old C"] {
            previous.bus.publish(event(title)).unwrap();
        }
        let old_cursor = 3;
        hub.invalidate(7);
        let current = hub.ensure(7).unwrap();
        for title in ["new A", "new B", "new C", "new D"] {
            current.bus.publish(event(title)).unwrap();
        }
        let same_generation = current.generation == previous_generation;
        let (_, replay, gap) = current.resume(Some(&previous_generation), Some(old_cursor));
        let replay_len = replay.len();
        hub.shutdown().await;
        assert!(
            !same_generation || gap || replay_len == 4,
            "Recreated bus reused generation; old cursor silently discarded {} new events",
            4 - replay_len
        );
    }
}
