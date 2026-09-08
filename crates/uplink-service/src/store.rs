use crate::crypto::Secret;
use sha2::{Digest, Sha256};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::Semaphore;
use tokio_postgres::{Client, NoTls, Row, types::ToSql};

pub struct Store {
    client: Client,
    ready: Arc<AtomicBool>,
    slots: Semaphore,
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
        let (client, driver) =
            tokio::time::timeout(Duration::from_secs(10), connection.connect(NoTls))
                .await
                .map_err(|_| "Datenbankstart hat die Frist überschritten.")?
                .map_err(|_| "Datenbank ist nicht verfügbar.")?;
        let ready = Arc::new(AtomicBool::new(true));
        let flag = ready.clone();
        tokio::spawn(async move {
            let _ = driver.await;
            flag.store(false, Ordering::Release);
        });
        Ok(Self {
            client,
            ready,
            slots: Semaphore::new(max_queries as usize),
        })
    }
    pub fn ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }
    pub async fn query(
        &self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Vec<Row>, &'static str> {
        let _slot = self
            .slots
            .try_acquire()
            .map_err(|_| "Datenbank ist ausgelastet.")?;
        tokio::time::timeout(Duration::from_secs(10), self.client.query(sql, params))
            .await
            .map_err(|_| "Datenbankanfrage hat die Frist überschritten.")?
            .map_err(|_| "Datenbankanfrage fehlgeschlagen.")
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
