use crate::crypto::Secret;
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};
use tokio::sync::Semaphore;
use tokio::time::{Instant, timeout_at};
use tokio_postgres::{NoTls, Row, types::ToSql};

tokio::task_local! { pub static REQUEST_DEADLINE: Instant; }
pub const CLEANUP_GRACE: Duration = Duration::from_secs(2);
const QUERY_LIMIT: Duration = Duration::from_secs(10);

pub struct Store {
    connection: tokio_postgres::Config,
    slots: Arc<Semaphore>,
}
impl Store {
    pub async fn connect(database: &Secret, max_queries: u32) -> Result<Self, &'static str> {
        let connection: tokio_postgres::Config = std::str::from_utf8(database.expose())
            .map_err(|_| "Datenbankzugang ist ungültig.")?
            .parse()
            .map_err(|_| "Datenbankzugang ist ungültig.")?;
        if connection.get_hosts().is_empty()
            || connection.get_hosts().iter().any(|host| match host {
                tokio_postgres::config::Host::Tcp(name) => {
                    !matches!(name.as_str(), "127.0.0.1" | "localhost" | "::1")
                }
                tokio_postgres::config::Host::Unix(_) => false,
            })
            || connection
                .get_hostaddrs()
                .iter()
                .any(|address| !address.is_loopback())
            || connection.get_user().is_none()
            || connection.get_dbname().is_none()
        {
            return Err("Datenbank benötigt eine explizite lokale Verbindung.");
        }
        if !(1..=64).contains(&max_queries) {
            return Err("Datenbankgrenze ist ungültig.");
        }
        let store = Self {
            connection,
            slots: Arc::new(Semaphore::new(max_queries as usize)),
        };
        store.query("SELECT 1", &[]).await?;
        Ok(store)
    }
    pub async fn ready(&self) -> bool {
        self.query("SELECT 1", &[]).await.is_ok()
    }
    pub async fn query(
        &self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Vec<Row>, &'static str> {
        self.query_with_retention(sql, params, None).await
    }
    pub(crate) async fn query_with_retention(
        &self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
        retention: Option<Arc<dyn std::any::Any + Send + Sync>>,
    ) -> Result<Vec<Row>, &'static str> {
        let slot = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| "Datenbank ist ausgelastet.")?;
        let deadline = REQUEST_DEADLINE
            .try_with(|value| *value)
            .unwrap_or_else(|_| Instant::now() + QUERY_LIMIT)
            .min(Instant::now() + QUERY_LIMIT);
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("Datenbankanfrage hat die Frist überschritten.");
        }
        let millis = remaining.as_millis().max(1);
        let mut config = self.connection.clone();
        // Serverfristen gelten auch nach einem abgebrochenen HTTP-Future.
        // Jede Operation hat eine eigene Transaktion und Verbindung; andere
        // Requests können deshalb keinen fremden CancelRequest abbekommen.
        config.options(format!("-c statement_timeout={millis} -c lock_timeout={millis} -c idle_in_transaction_session_timeout={millis}"));
        let (mut client, driver) = timeout_at(deadline, config.connect(NoTls))
            .await
            .map_err(|_| "Datenbankanfrage hat die Frist überschritten.")?
            .map_err(|_| "Datenbank ist nicht verfügbar.")?;
        // Der Treiber hält das Permit auch beim Abbruch des Aufrufers: der
        // Transaction-Drop sendet ROLLBACK, der Client-Drop schließt danach.
        // Nach der begrenzten Nachlaufzeit schließt Drop die Verbindung hart.
        let driver = tokio::spawn(async move {
            let _slot = slot;
            let _retention = retention;
            let _ = timeout_at(deadline + CLEANUP_GRACE, driver).await;
        });
        let result = async {
            let cancel = client.cancel_token();
            let transaction = timeout_at(deadline, client.transaction()).await
                .map_err(|_| "Datenbankanfrage hat die Frist überschritten.")?
                .map_err(|_| "Datenbanktransaktion konnte nicht starten.")?;
            let rows = {
                let query = transaction.query(sql, params);
                tokio::pin!(query);
                match timeout_at(deadline, &mut query).await {
                    Ok(Ok(rows)) => rows,
                    Ok(Err(_)) => {
                        return Err("Datenbankanfrage fehlgeschlagen oder Frist überschritten.");
                    }
                    Err(_) => {
                        // Nicht nur das Client-Future fallenlassen: laufendes
                        // Statement abbrechen und sein Ergebnis begrenzt abholen.
                        let cleanup = deadline + CLEANUP_GRACE;
                        let _ = timeout_at(cleanup, cancel.cancel_query(NoTls)).await;
                        let _ = timeout_at(cleanup, &mut query).await;
                        return Err("Datenbankanfrage hat die Frist überschritten.");
                    }
                }
            };
            if Instant::now() >= deadline {
                return Err("Datenbankanfrage hat die Frist überschritten.");
            }
            timeout_at(deadline, transaction.commit()).await
                .map_err(|_| "Datenbankabschluss ist unklar; gespeicherten Stand vor Wiederholung prüfen.")?
                .map_err(|_| "Datenbankabschluss ist unklar; gespeicherten Stand vor Wiederholung prüfen.")?;
            Ok(rows)
        }.await;
        drop(client);
        let _ = driver.await;
        result
    }
    pub async fn authenticate_ingest(&self, key: &str) -> Result<u64, &'static str> {
        if key.len() != 36
            || !key.starts_with("rsr_")
            || !key[4..]
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err("Streamzugang ist ungültig.");
        }
        let hash = hex::encode(Sha256::digest(key.as_bytes()));
        let rows = self
            .query(
                "SELECT streamer_id FROM relay.users WHERE ingest_key_hash=$1 AND enabled=true",
                &[&hash],
            )
            .await?;
        let id: i64 = rows
            .first()
            .ok_or("Streamzugang wurde abgewiesen.")?
            .try_get(0)
            .map_err(|_| "Nutzeridentität ist ungültig.")?;
        u64::try_from(id)
            .ok()
            .filter(|id| *id > 0)
            .ok_or("Nutzeridentität ist ungültig.")
    }
}
