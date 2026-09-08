use crate::{
    Cipher, Error, ExportFile, Result, Settings, Storage, Store, TwitchBinding,
    store::Job,
    youtube::{CHUNK_BYTES, Processing, UploadReply, YouTube},
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

#[derive(Debug, Clone)]
pub struct SourceRequest {
    pub session_id: i64,
    pub streamer_id: u64,
    pub ended_at: DateTime<Utc>,
    pub settings: Settings,
    pub object_id: Option<String>,
    pub manifest: Option<Value>,
    pub twitch_binding: Option<TwitchBinding>,
}
#[derive(Debug, Clone)]
pub struct PreparedSource {
    pub object_id: String,
    pub manifest: Value,
    pub export: ExportFile,
}
#[async_trait]
pub trait SourceProvider: Send + Sync {
    async fn prepare(&self, source: SourceRequest) -> Result<PreparedSource>;
}
pub struct Worker {
    store: Store,
    storage: Storage,
    source: Arc<dyn SourceProvider>,
    youtube: Arc<YouTube>,
    cipher: Arc<dyn Cipher>,
    upload_bytes_per_second: u64,
    slot: tokio::sync::Semaphore,
}
impl Worker {
    pub fn new(
        store: Store,
        storage: Storage,
        source: Arc<dyn SourceProvider>,
        youtube: Arc<YouTube>,
        cipher: Arc<dyn Cipher>,
    ) -> Self {
        Self {
            store,
            storage,
            source,
            youtube,
            cipher,
            upload_bytes_per_second: 1024 * 1024,
            slot: tokio::sync::Semaphore::new(1),
        }
    }
    pub fn with_upload_budget(mut self, bytes_per_second: u64) -> Result<Self> {
        if bytes_per_second == 0 || bytes_per_second > 1024 * 1024 * 1024 {
            return Err(Error::Invalid);
        }
        self.upload_bytes_per_second = bytes_per_second;
        Ok(self)
    }
    /// Führt einen fälligen dauerhaften Auftrag aus. Cancel lässt die Lease auslaufen;
    /// der nächste Worker gleicht die gespeicherte Uploadsession zuerst ab.
    pub async fn run_one(&self) -> Result<bool> {
        let Ok(_slot) = self.slot.try_acquire() else {
            return Ok(false);
        };
        self.cleanup_one().await?;
        let Some(job) = self.store.claim().await? else {
            return Ok(false);
        };
        let step = self.step(&job);
        tokio::pin!(step);
        let mut heartbeat = tokio::time::interval(Duration::from_secs(30));
        heartbeat.tick().await;
        let result = loop {
            tokio::select! {result=&mut step=>break result,_=heartbeat.tick()=>self.store.heartbeat(&job).await?}
        };
        match result {
            Ok(delay) => self.store.release(&job, delay).await?,
            Err(Error::LeaseLost) => return Err(Error::LeaseLost),
            Err(error) => self.store.error(&job, error).await?,
        }
        Ok(true)
    }
    async fn step(&self, job: &Job) -> Result<i32> {
        match job.state.as_str() {
            "waiting_source" => {
                let object=self.store.db.query("SELECT id,manifest,complete FROM relay.vod_objects WHERE session_id=$1 AND streamer_id=$2 AND state='available'",&[&job.session_id,&(job.streamer_id as i64)]).await?;
                let binding=self.store.db.query("SELECT binding FROM relay.vod_twitch_bindings WHERE session_id=$1 AND streamer_id=$2 AND conflicted=false",&[&job.session_id,&(job.streamer_id as i64)]).await?;
                let prepared = self
                    .source
                    .prepare(SourceRequest {
                        session_id: job.session_id,
                        streamer_id: job.streamer_id,
                        ended_at: job.ended_at,
                        settings: job.settings.clone(),
                        object_id: object.first().map(|r| r.get(0)),
                        manifest: object.first().map(|r| r.get(1)),
                        twitch_binding: binding
                            .first()
                            .map(|r| {
                                serde_json::from_value(r.get(0)).map_err(|_| Error::UnboundTwitch)
                            })
                            .transpose()?,
                    })
                    .await?;
                let total = i64::try_from(prepared.export.bytes).map_err(|_| Error::StorageFull)?;
                if total <= 0 {
                    return Err(Error::Incomplete);
                }
                let mut manifest = prepared.manifest;
                manifest["export"] = json!(prepared.export);
                // Mit der geleasten Jobzeile verriegeln: veraltete Worker dürfen kein Objekt austauschen.
                self.store.fenced(job,"WITH held AS (SELECT id,session_id,streamer_id FROM relay.vod_jobs WHERE id=$1 AND lease_owner=$2 AND lease_until>now() FOR UPDATE), saved AS (INSERT INTO relay.vod_objects(id,session_id,streamer_id,manifest,complete) SELECT $3,session_id,streamer_id,$4,true FROM held ON CONFLICT(session_id) DO UPDATE SET manifest=excluded.manifest,complete=true WHERE relay.vod_objects.id=excluded.id AND relay.vod_objects.streamer_id=excluded.streamer_id AND relay.vod_objects.state='available' RETURNING id) UPDATE relay.vod_jobs SET state='prepared',object_id=saved.id,total_bytes=$5,last_error=NULL FROM saved WHERE relay.vod_jobs.id=$1 RETURNING relay.vod_jobs.id",&[&prepared.object_id,&manifest,&total]).await?;
                Ok(0)
            }
            "prepared" => {
                self.youtube
                    .verify_channel(job.streamer_id, &job.settings.youtube_channel_id)
                    .await?;
                let total = u64::try_from(job.total_bytes.ok_or(Error::Incomplete)?)
                    .map_err(|_| Error::Incomplete)?;
                self.store.fenced(job,"UPDATE relay.vod_jobs SET state='starting' WHERE id=$1 AND lease_owner=$2 AND lease_until>now() AND state='prepared' RETURNING id",&[]).await?;
                match self
                    .youtube
                    .start(job.streamer_id, &job.settings, total)
                    .await
                {
                    Ok(session) => {
                        self.store
                            .upload_started(job, self.cipher.as_ref(), &session)
                            .await?
                    }
                    Err(error) => {
                        if matches!(
                            error,
                            Error::NeedsReauth
                                | Error::Quota
                                | Error::Disconnected
                                | Error::WrongChannel
                                | Error::BrokerUnavailable
                        ) {
                            self.store.fenced(job,"UPDATE relay.vod_jobs SET state='prepared' WHERE id=$1 AND lease_owner=$2 AND lease_until>now() AND state='starting' RETURNING id",&[]).await?;
                            return Err(error);
                        }
                        return Err(Error::Ambiguous);
                    }
                }
                Ok(0)
            }
            "starting" => Err(Error::Ambiguous),
            "uploading" => {
                let object = job.object_id.as_ref().ok_or(Error::Incomplete)?;
                let total = u64::try_from(job.total_bytes.ok_or(Error::Incomplete)?)
                    .map_err(|_| Error::Incomplete)?;
                let rows=self.store.db.query("SELECT manifest->'export' FROM relay.vod_objects WHERE id=$1 AND streamer_id=$2 AND complete=true AND state='available'",&[&object,&(job.streamer_id as i64)]).await?;
                let expected: ExportFile =
                    serde_json::from_value(rows.first().ok_or(Error::Storage)?.get(0))
                        .map_err(|_| Error::Storage)?;
                let actual = self.storage.digest(object, "export.mp4").await?;
                if actual.bytes != total
                    || actual.bytes != expected.bytes
                    || actual.sha256 != expected.sha256
                {
                    return Err(Error::Storage);
                }
                let session = self.cipher.open(
                    job.upload_session_enc.as_deref().ok_or(Error::Ambiguous)?,
                    &job.aad(),
                )?;
                let mut reply = self
                    .youtube
                    .reconcile(
                        job.streamer_id,
                        &job.settings.youtube_channel_id,
                        &session,
                        total,
                    )
                    .await?;
                let mut file = self.storage.open(object, "export.mp4").await?;
                loop {
                    match reply {
                        UploadReply::Complete(ref video_id) => {
                            self.store.fenced(job,"UPDATE relay.vod_jobs SET state='processing',confirmed_bytes=total_bytes,video_id=$3,last_error=NULL WHERE id=$1 AND lease_owner=$2 AND lease_until>now() AND state='uploading' RETURNING id",&[video_id]).await?;
                            return Ok(0);
                        }
                        UploadReply::Offset(offset) => {
                            self.store.fenced(job,"UPDATE relay.vod_jobs SET confirmed_bytes=$3,last_error=NULL WHERE id=$1 AND lease_owner=$2 AND lease_until>now() AND state='uploading' RETURNING id",&[&(offset as i64)]).await?;
                            if offset >= total {
                                return Err(Error::Ambiguous);
                            }
                            file.seek(std::io::SeekFrom::Start(offset))
                                .await
                                .map_err(|_| Error::Storage)?;
                            let mut data =
                                vec![0; ((total - offset).min(CHUNK_BYTES as u64)) as usize];
                            file.read_exact(&mut data)
                                .await
                                .map_err(|_| Error::Storage)?;
                            let pacing = tokio::time::Instant::now()
                                + Duration::from_secs_f64(
                                    data.len() as f64 / self.upload_bytes_per_second as f64,
                                );
                            reply = self
                                .youtube
                                .chunk(
                                    job.streamer_id,
                                    &job.settings.youtube_channel_id,
                                    &session,
                                    total,
                                    offset,
                                    data,
                                )
                                .await?;
                            tokio::time::sleep_until(pacing).await;
                        }
                    }
                }
            }
            "processing" => {
                let video = job.video_id.as_ref().ok_or(Error::Ambiguous)?;
                match self
                    .youtube
                    .processing(job.streamer_id, &job.settings.youtube_channel_id, video)
                    .await?
                {
                    Processing::Pending => Ok(30),
                    Processing::Succeeded => {
                        self.store.fenced(job,"UPDATE relay.vod_jobs SET state='ready',processing_succeeded=true,last_error=NULL WHERE id=$1 AND lease_owner=$2 AND lease_until>now() AND state='processing' AND confirmed_bytes=total_bytes AND video_id IS NOT NULL RETURNING id",&[]).await?;
                        Ok(0)
                    }
                }
            }
            _ => Err(Error::Invalid),
        }
    }
    /// Dauerhafter Löschzustand überlebt einen Abbruch zwischen Dateilöschung und DB-Abschluss.
    pub async fn cleanup_one(&self) -> Result<bool> {
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|_| Error::Storage)?;
        let owner = hex::encode(random);
        let rows=self.store.db.query("WITH candidate AS (SELECT o.id FROM relay.vod_objects o WHERE o.state IN ('available','deleting') AND (o.cleanup_until IS NULL OR o.cleanup_until<now()) AND o.complete=true AND EXISTS(SELECT 1 FROM relay.vod_jobs j WHERE j.object_id=o.id AND j.streamer_id=o.streamer_id AND j.state='ready' AND j.processing_succeeded=true AND j.video_id IS NOT NULL AND j.confirmed_bytes=j.total_bytes) AND NOT EXISTS(SELECT 1 FROM relay.vod_jobs j WHERE j.object_id=o.id AND (j.streamer_id<>o.streamer_id OR j.state<>'ready' OR j.processing_succeeded=false)) ORDER BY o.created_at FOR UPDATE OF o SKIP LOCKED LIMIT 1) UPDATE relay.vod_objects o SET state='deleting',cleanup_owner=$1,cleanup_until=now()+interval '120 seconds' FROM candidate WHERE o.id=candidate.id RETURNING o.id",&[&owner]).await?;
        let Some(row) = rows.first() else {
            return Ok(false);
        };
        let id: String = row.get(0);
        self.storage.delete_object(&id).await?;
        self.store.db.query("UPDATE relay.vod_objects SET state='deleted',cleanup_owner=NULL,cleanup_until=NULL WHERE id=$1 AND state='deleting' AND cleanup_owner=$2",&[&id,&owner]).await?;
        Ok(true)
    }
}
