use crate::{Cipher, EndReason, Error, LogicalSessionEnded, Result, Settings, TwitchBinding};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tokio_postgres::{Row, Transaction, types::ToSql};

pub const MIGRATION: &str = include_str!("../migrations/202609080001_vod.sql");
/// Dienstadapter verwendet seine vorhandenen Verbindungen, Grenzen und Secrets.
#[async_trait]
pub trait Database: Send + Sync {
    async fn query(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Vec<Row>>;
}
#[derive(Clone)]
pub struct Store {
    pub(crate) db: Arc<dyn Database>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobStatus {
    pub id: i64,
    pub session_id: i64,
    pub state: String,
    pub confirmed_bytes: i64,
    pub total_bytes: Option<i64>,
    pub video_id: Option<String>,
    pub last_error: Option<String>,
    pub processing_succeeded: bool,
}
pub(crate) struct Job {
    pub id: i64,
    pub session_id: i64,
    pub streamer_id: u64,
    pub state: String,
    pub settings: Settings,
    pub object_id: Option<String>,
    pub upload_session_enc: Option<Vec<u8>>,
    pub total_bytes: Option<i64>,
    pub video_id: Option<String>,
    pub lease_owner: String,
    pub ended_at: DateTime<Utc>,
}
impl Job {
    pub fn aad(&self) -> String {
        format!(
            "uplink-vod:streamer:{}:job:{}:upload-session:v1",
            self.streamer_id, self.id
        )
    }
}
fn id(streamer: u64) -> Result<i64> {
    i64::try_from(streamer)
        .ok()
        .filter(|id| *id > 0)
        .ok_or(Error::Invalid)
}
fn json<T: Serialize>(value: &T) -> Result<Value> {
    serde_json::to_value(value).map_err(|_| Error::Invalid)
}
impl Store {
    pub fn new(db: Arc<dyn Database>) -> Self {
        Self { db }
    }
    pub async fn settings(&self, streamer: u64) -> Result<Option<Settings>> {
        let rows = self
            .db
            .query(
                "SELECT settings FROM relay.vod_settings WHERE streamer_id=$1",
                &[&id(streamer)?],
            )
            .await?;
        rows.first()
            .map(|r| serde_json::from_value(r.get(0)).map_err(|_| Error::Database))
            .transpose()
    }
    /// Aufrufer muss vorher Kanal über den Broker/YouTube bestätigen.
    pub async fn save_settings(&self, streamer: u64, settings: &Settings) -> Result<()> {
        settings.validate()?;
        self.db.query("INSERT INTO relay.vod_settings(streamer_id,settings) VALUES($1,$2) ON CONFLICT(streamer_id) DO UPDATE SET settings=excluded.settings,updated_at=now()",&[&id(streamer)?,&json(settings)?]).await?;
        Ok(())
    }
    /// Innerhalb derselben Diensttransaktion wie der dauerhafte Sessionabschluss aufrufen.
    pub async fn enqueue_in_transaction(
        tx: &Transaction<'_>,
        event: &LogicalSessionEnded,
    ) -> Result<()> {
        let streamer = id(event.streamer_id)?;
        if event.session_id <= 0 {
            return Err(Error::Invalid);
        }
        let reason = match event.reason {
            EndReason::ExplicitStop => "explicit_stop",
            EndReason::ReconnectExpired => "reconnect_expired",
            EndReason::ControlledStop => "controlled_stop",
        };
        tx.execute("INSERT INTO relay.vod_jobs(session_id,streamer_id,ended_at,end_reason,settings) SELECT $1,$2,$3,$4,settings FROM relay.vod_settings cfg JOIN relay.sessions session ON session.id=$1 AND session.streamer_id=cfg.streamer_id WHERE cfg.streamer_id=$2 AND settings->>'enabled'='true' ON CONFLICT(session_id) DO NOTHING",&[&event.session_id,&streamer,&event.ended_at,&reason]).await.map_err(|_|Error::Database)?;
        Ok(())
    }
    pub async fn bind_twitch_stream(
        tx: &Transaction<'_>,
        session: i64,
        streamer: u64,
        binding: &TwitchBinding,
    ) -> Result<()> {
        binding.validate_identity()?;
        // Ein verlorener Mix oder eine andere Twitch-Session darf nicht durch einen
        // späteren Reconnect als durchgehend korrekt überschrieben werden.
        let changed=tx.execute("INSERT INTO relay.vod_twitch_bindings(session_id,streamer_id,binding) SELECT $1,$2,$3 FROM relay.sessions WHERE id=$1 AND streamer_id=$2 ON CONFLICT(session_id) DO UPDATE SET conflicted=relay.vod_twitch_bindings.conflicted OR relay.vod_twitch_bindings.binding->>'stream_id'<>excluded.binding->>'stream_id' OR relay.vod_twitch_bindings.binding->>'broadcaster_id'<>excluded.binding->>'broadcaster_id', binding=jsonb_set(relay.vod_twitch_bindings.binding,'{vod_audio_confirmed}',to_jsonb((relay.vod_twitch_bindings.binding->>'vod_audio_confirmed')::boolean AND (excluded.binding->>'vod_audio_confirmed')::boolean)) WHERE relay.vod_twitch_bindings.streamer_id=excluded.streamer_id",&[&session,&id(streamer)?,&json(binding)?]).await.map_err(|_|Error::Database)?;
        if changed != 1 {
            return Err(Error::UnboundTwitch);
        }
        Ok(())
    }
    pub async fn register_object(
        &self,
        session: i64,
        streamer: u64,
        object: &str,
        manifest: &Value,
        complete: bool,
    ) -> Result<()> {
        validate_object_id(object)?;
        let rows=self.db.query("INSERT INTO relay.vod_objects(id,session_id,streamer_id,manifest,complete) SELECT $1,$2,$3,$4,$5 FROM relay.sessions WHERE id=$2 AND streamer_id=$3 ON CONFLICT(id) DO UPDATE SET manifest=excluded.manifest,complete=excluded.complete WHERE relay.vod_objects.session_id=excluded.session_id AND relay.vod_objects.streamer_id=excluded.streamer_id AND relay.vod_objects.complete=false AND relay.vod_objects.state='available' RETURNING id",&[&object,&session,&id(streamer)?,&manifest,&complete]).await?;
        if rows.len() != 1 {
            return Err(Error::Storage);
        }
        Ok(())
    }
    pub async fn statuses(&self, streamer: u64) -> Result<Vec<JobStatus>> {
        self.db.query("SELECT id,session_id,state,confirmed_bytes,total_bytes,video_id,last_error,processing_succeeded FROM relay.vod_jobs WHERE streamer_id=$1 ORDER BY id DESC LIMIT 100",&[&id(streamer)?]).await?.into_iter().map(|r|Ok(JobStatus{id:r.try_get(0).map_err(|_|Error::Database)?,session_id:r.get(1),state:r.get(2),confirmed_bytes:r.get(3),total_bytes:r.get(4),video_id:r.get(5),last_error:r.get(6),processing_succeeded:r.get(7)})).collect()
    }
    /// Autorisierte Wiederaufnahme benötigt keine neue Uploadsession. Ambiguous bleibt gesperrt.
    pub async fn retry(&self, streamer: u64, job: i64) -> Result<()> {
        let rows=self.db.query("UPDATE relay.vod_jobs SET state=resume_state,resume_state=NULL,last_error=NULL,next_attempt_at=now() WHERE id=$1 AND streamer_id=$2 AND state='blocked' AND resume_state IS NOT NULL AND last_error <> 'ambiguous' AND lease_owner IS NULL RETURNING id",&[&job,&id(streamer)?]).await?;
        if rows.len() != 1 {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    pub(crate) async fn claim(&self) -> Result<Option<Job>> {
        let mut random = [0; 16];
        getrandom::fill(&mut random).map_err(|_| Error::Invalid)?;
        let owner = hex::encode(random);
        let rows=self.db.query("WITH next AS (SELECT id FROM relay.vod_jobs WHERE state NOT IN ('ready','blocked','failed','cancelled') AND next_attempt_at<=now() AND (lease_until IS NULL OR lease_until<now()) ORDER BY next_attempt_at,id FOR UPDATE SKIP LOCKED LIMIT 1) UPDATE relay.vod_jobs j SET lease_owner=$1,lease_until=now()+interval '120 seconds',attempts=attempts+1,updated_at=now() FROM next WHERE j.id=next.id RETURNING j.id,j.session_id,j.streamer_id,j.state,j.settings,j.object_id,j.upload_session_enc,j.total_bytes,j.video_id,j.ended_at",&[&owner]).await?;
        let Some(r) = rows.first() else {
            return Ok(None);
        };
        Ok(Some(Job {
            id: r.get(0),
            session_id: r.get(1),
            streamer_id: u64::try_from(r.get::<_, i64>(2)).map_err(|_| Error::Database)?,
            state: r.get(3),
            settings: serde_json::from_value(r.get(4)).map_err(|_| Error::Database)?,
            object_id: r.get(5),
            upload_session_enc: r.get(6),
            total_bytes: r.get(7),
            video_id: r.get(8),
            lease_owner: owner,
            ended_at: r.get(9),
        }))
    }
    pub(crate) async fn fenced(
        &self,
        job: &Job,
        sql: &str,
        values: &[&(dyn ToSql + Sync)],
    ) -> Result<Vec<Row>> {
        let mut args: Vec<&(dyn ToSql + Sync)> = vec![&job.id, &job.lease_owner];
        args.extend_from_slice(values);
        let rows = self.db.query(sql, &args).await?;
        if rows.is_empty() {
            return Err(Error::LeaseLost);
        }
        Ok(rows)
    }
    pub(crate) async fn heartbeat(&self, job: &Job) -> Result<()> {
        self.fenced(job,"UPDATE relay.vod_jobs SET lease_until=now()+interval '120 seconds' WHERE id=$1 AND lease_owner=$2 AND lease_until>now() RETURNING id",&[]).await?;
        Ok(())
    }
    pub(crate) async fn release(&self, job: &Job, delay: i32) -> Result<()> {
        self.fenced(job,"UPDATE relay.vod_jobs SET lease_owner=NULL,lease_until=NULL,next_attempt_at=now()+($3::integer*interval '1 second'),updated_at=now() WHERE id=$1 AND lease_owner=$2 AND lease_until>now() RETURNING id",&[&delay]).await?;
        Ok(())
    }
    pub(crate) async fn error(&self, job: &Job, error: Error) -> Result<()> {
        let code = serde_json::to_value(error)
            .map_err(|_| Error::Invalid)?
            .as_str()
            .ok_or(Error::Invalid)?
            .to_owned();
        let terminal = matches!(error, Error::ProcessingFailed | Error::Incomplete);
        let blocked = matches!(
            error,
            Error::NeedsReauth
                | Error::Disconnected
                | Error::WrongChannel
                | Error::Ambiguous
                | Error::MissingVodAudio
                | Error::UnboundTwitch
                | Error::SourceUnavailable
                | Error::StorageFull
                | Error::Protocol
                | Error::ChangedProfile
                | Error::Invalid
        );
        let state = if terminal {
            "failed"
        } else if blocked {
            "blocked"
        } else {
            "retry"
        };
        self.fenced(job,"UPDATE relay.vod_jobs SET resume_state=CASE WHEN $3='blocked' THEN state ELSE resume_state END,state=CASE WHEN $3='retry' THEN state ELSE $3 END,last_error=$4,lease_owner=NULL,lease_until=NULL,next_attempt_at=now()+interval '60 seconds',updated_at=now() WHERE id=$1 AND lease_owner=$2 AND lease_until>now() RETURNING id",&[&state,&code]).await?;
        Ok(())
    }
    pub(crate) async fn upload_started(
        &self,
        job: &Job,
        cipher: &dyn Cipher,
        uri: &crate::Secret,
    ) -> Result<()> {
        let sealed = cipher.seal(uri.expose().as_bytes(), &job.aad())?;
        self.fenced(job,"UPDATE relay.vod_jobs SET state='uploading',upload_session_enc=$3,confirmed_bytes=0,last_error=NULL WHERE id=$1 AND lease_owner=$2 AND lease_until>now() AND state='starting' RETURNING id",&[&sealed]).await?;
        Ok(())
    }
}
pub(crate) fn validate_object_id(value: &str) -> Result<()> {
    if value.len() != 32
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Storage);
    }
    Ok(())
}
