use crate::{
    api::{ApiResult, ServiceState, TenantQuery, authorize, failure},
    store::CheckedStatement,
};
use axum::{
    Json, Router,
    extract::{
        Path, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::{delete, get, post, put},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;

const MAX_SOURCES: i64 = 8;
const MAX_SCENES: i64 = 24;

pub(crate) fn router(state: Arc<ServiceState>) -> Router {
    Router::new()
        .route("/v1/me/cast", get(studio))
        .route("/v1/me/cast/sources", post(create_source))
        .route("/v1/me/cast/sources/{source_id}", delete(delete_source))
        .route(
            "/v1/me/cast/sources/{source_id}/rotate",
            post(rotate_source),
        )
        .route(
            "/v1/me/cast/sources/{source_id}/preview",
            get(preview_socket),
        )
        .route("/v1/me/cast/scenes", post(create_scene))
        .route("/v1/me/cast/scenes/{scene_id}", delete(delete_scene))
        .route("/v1/me/cast/preview", put(select_preview))
        .route("/v1/me/cast/program", put(select_program))
        .with_state(state)
}

fn valid_name(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty() && value.chars().count() <= 80 && !value.chars().any(char::is_control))
        .then_some(value)
}

fn source_key() -> Result<zeroize::Zeroizing<String>, &'static str> {
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| "Quellzugang konnte nicht erzeugt werden.")?;
    Ok(zeroize::Zeroizing::new(format!(
        "cst_{}",
        hex::encode(random)
    )))
}

fn key_hash(key: &str) -> String {
    hex::encode(Sha256::digest(key.as_bytes()))
}

fn source_binding(streamer_id: i64, hash: &str) -> String {
    format!("cast_source:{streamer_id}:{hash}")
}

fn invalid_cast_data() -> (StatusCode, Json<Value>) {
    failure(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Cast-Studio-Daten sind ungültig.",
    )
}

async fn studio(
    State(state): State<Arc<ServiceState>>,
    headers: HeaderMap,
    Query(query): Query<TenantQuery>,
) -> ApiResult {
    authorize(&state, &headers, query.streamer_id)?;
    let tenant = u64::try_from(query.streamer_id).map_err(|_| invalid_cast_data())?;
    let source_rows = state
        .store
        .query(
            "SELECT source_id,label,kind,ingest_key_hash,ingest_key_enc,enabled FROM relay.cast_sources WHERE streamer_id=$1 ORDER BY source_id",
            &[&query.streamer_id],
        )
        .await
        .map_err(|error| failure(StatusCode::SERVICE_UNAVAILABLE, error))?;
    let scene_rows = state
        .store
        .query(
            "SELECT scene_id,name,source_id,sort_order FROM relay.cast_scenes WHERE streamer_id=$1 ORDER BY sort_order,scene_id",
            &[&query.streamer_id],
        )
        .await
        .map_err(|error| failure(StatusCode::SERVICE_UNAVAILABLE, error))?;
    let state_rows = state
        .store
        .query(
            "SELECT program_scene_id,preview_scene_id,switch_generation FROM relay.cast_state WHERE streamer_id=$1",
            &[&query.streamer_id],
        )
        .await
        .map_err(|error| failure(StatusCode::SERVICE_UNAVAILABLE, error))?;

    let sessions = state.registry.status(tenant);
    let mut sources = Vec::with_capacity(source_rows.len());
    for row in source_rows {
        let source_id: i64 = row.try_get(0).map_err(|_| invalid_cast_data())?;
        let source_id_u = u64::try_from(source_id)
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(invalid_cast_data)?;
        let label: String = row.try_get(1).map_err(|_| invalid_cast_data())?;
        let kind: String = row.try_get(2).map_err(|_| invalid_cast_data())?;
        let hash: String = row.try_get(3).map_err(|_| invalid_cast_data())?;
        let encrypted: Vec<u8> = row.try_get(4).map_err(|_| invalid_cast_data())?;
        let enabled: bool = row.try_get(5).map_err(|_| invalid_cast_data())?;
        let key = state
            .secrets
            .encryption
            .open(&encrypted, &source_binding(query.streamer_id, &hash))
            .map_err(|error| failure(StatusCode::SERVICE_UNAVAILABLE, error))?;
        let key = std::str::from_utf8(key.expose()).map_err(|_| invalid_cast_data())?;
        if key_hash(key) != hash || !key.starts_with("cst_") {
            return Err(invalid_cast_data());
        }
        let session = sessions
            .iter()
            .find(|session| session.active && session.source_id == Some(source_id_u))
            .map(crate::media_status::session_status)
            .unwrap_or(Value::Null);
        sources.push(json!({
            "source_id": source_id_u,
            "label": label,
            "kind": kind,
            "enabled": enabled,
            "ingest_key": key,
            "ingest_url": state.config.public_ingest_url,
            "session": session,
        }));
    }

    let mut scenes = Vec::with_capacity(scene_rows.len());
    for row in scene_rows {
        let scene_id: i64 = row.try_get(0).map_err(|_| invalid_cast_data())?;
        let source_id: i64 = row.try_get(2).map_err(|_| invalid_cast_data())?;
        scenes.push(json!({
            "scene_id": u64::try_from(scene_id).map_err(|_| invalid_cast_data())?,
            "name": row.try_get::<_, String>(1).map_err(|_| invalid_cast_data())?,
            "source_id": u64::try_from(source_id).map_err(|_| invalid_cast_data())?,
            "sort_order": row.try_get::<_, i32>(3).map_err(|_| invalid_cast_data())?,
        }));
    }
    let (program_scene_id, preview_scene_id, switch_generation) = match state_rows.first() {
        Some(row) => (
            row.try_get::<_, Option<i64>>(0)
                .map_err(|_| invalid_cast_data())?
                .and_then(|value| u64::try_from(value).ok()),
            row.try_get::<_, Option<i64>>(1)
                .map_err(|_| invalid_cast_data())?
                .and_then(|value| u64::try_from(value).ok()),
            row.try_get::<_, i64>(2).map_err(|_| invalid_cast_data())?,
        ),
        None => (None, None, 0),
    };
    Ok(Json(json!({
        "sources": sources,
        "scenes": scenes,
        "program_scene_id": program_scene_id,
        "preview_scene_id": preview_scene_id,
        "switch_generation": switch_generation,
        "limits": {"sources": MAX_SOURCES, "scenes": MAX_SCENES, "online_sources": state.config.cast_session_limit()},
        "preview": {
            "available": true,
            "strategy": "compressed_browser_decode",
            "codecs": ["h264"],
            "audio": false,
            "server_encode": false,
            "standby_output": "drop"
        },
        "program_switch": {
            "available": true,
            "transport": "persistent_program_mux",
            "persistent_connection": true,
            "switch_mode": "next_keyframe",
            "requires_same_track_codecs": true,
            "seamless_media_mux": false
        }
    })))
}

fn preview_packet(event: &uplink_ingest::MediaEvent) -> Option<Vec<u8>> {
    use uplink_ingest::{EventKind, MediaKind, WireCodec};
    if event.identity.track.kind != MediaKind::Video || event.payload().len() > 4 * 1024 * 1024 {
        return None;
    }
    let event_kind = match event.event_kind {
        EventKind::SequenceHeader => 0_u8,
        EventKind::Frame => 1,
        EventKind::SequenceEnd => 2,
        EventKind::Metadata => return None,
    };
    let codec = match event.codec {
        WireCodec::H264 => 1_u8,
        WireCodec::Av1 => 2,
        WireCodec::Hevc => 3,
        WireCodec::Aac => return None,
    };
    let keyframe = event.event_kind == EventKind::Frame
        && event
            .wire_body()
            .first()
            .is_some_and(|first| first & 0x70 == 0x10);
    let mut packet = Vec::with_capacity(16 + event.payload().len());
    packet.extend_from_slice(&[1, event_kind, codec, u8::from(keyframe)]);
    packet.extend_from_slice(&event.dts_ms.to_be_bytes());
    packet.extend_from_slice(&event.pts_ms.to_be_bytes());
    packet.extend_from_slice(event.payload());
    Some(packet)
}

async fn serve_preview_socket(
    mut socket: WebSocket,
    mut subscription: crate::registry::CastPreviewSubscription,
) {
    if let Some(header) = subscription.video_header.take()
        && let Some(packet) = preview_packet(&header)
        && socket.send(Message::Binary(packet.into())).await.is_err()
    {
        return;
    }
    loop {
        let event = match subscription.receiver.recv().await {
            Ok(event) => event,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => return,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        };
        let Some(packet) = preview_packet(&event) else {
            continue;
        };
        if socket.send(Message::Binary(packet.into())).await.is_err() {
            return;
        }
    }
}

async fn preview_socket(
    State(state): State<Arc<ServiceState>>,
    headers: HeaderMap,
    Path(source_id): Path<u64>,
    Query(query): Query<TenantQuery>,
    upgrade: WebSocketUpgrade,
) -> Result<Response, (StatusCode, Json<Value>)> {
    authorize(&state, &headers, query.streamer_id)?;
    let tenant = u64::try_from(query.streamer_id)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| failure(StatusCode::BAD_REQUEST, "Nutzeridentität ist ungültig."))?;
    let source = i64::try_from(source_id)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| failure(StatusCode::BAD_REQUEST, "Quellenidentität ist ungültig."))?;
    let exists = state
        .store
        .query(
            "SELECT source_id FROM relay.cast_sources WHERE streamer_id=$1 AND source_id=$2 AND enabled=true",
            &[&query.streamer_id, &source],
        )
        .await
        .map_err(|error| failure(StatusCode::SERVICE_UNAVAILABLE, error))?;
    if exists.is_empty() {
        return Err(failure(
            StatusCode::NOT_FOUND,
            "Quelle wurde nicht gefunden.",
        ));
    }
    if !state.registry.source_active(tenant, source_id) {
        return Err(failure(
            StatusCode::CONFLICT,
            "Quelle ist offline; Vorschau startet erst nach einem verbundenen Eingang.",
        ));
    }
    let subscription = state
        .registry
        .subscribe_cast_preview(tenant, source_id)
        .ok_or_else(|| {
            failure(
                StatusCode::TOO_MANY_REQUESTS,
                "Für diese Quelle sind bereits zu viele Vorschaufenster geöffnet.",
            )
        })?;
    Ok(upgrade.on_upgrade(move |socket| serve_preview_socket(socket, subscription)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateSource {
    streamer_id: i64,
    label: String,
    kind: String,
}

async fn create_source(
    State(state): State<Arc<ServiceState>>,
    headers: HeaderMap,
    payload: Result<Json<CreateSource>, axum::extract::rejection::JsonRejection>,
) -> ApiResult {
    let Json(request) = payload.map_err(|_| {
        failure(
            StatusCode::BAD_REQUEST,
            "Anfrage enthält ungültige JSON-Daten.",
        )
    })?;
    authorize(&state, &headers, request.streamer_id)?;
    let label = valid_name(&request.label)
        .ok_or_else(|| failure(StatusCode::BAD_REQUEST, "Quellenname ist ungültig."))?;
    if !matches!(request.kind.as_str(), "pov" | "camera") {
        return Err(failure(StatusCode::BAD_REQUEST, "Quellentyp ist ungültig."));
    }
    let key = source_key().map_err(|error| failure(StatusCode::SERVICE_UNAVAILABLE, error))?;
    let hash = key_hash(&key);
    let encrypted = state
        .secrets
        .encryption
        .seal(key.as_bytes(), &source_binding(request.streamer_id, &hash))
        .map_err(|error| failure(StatusCode::SERVICE_UNAVAILABLE, error))?;
    let rows = state
        .store
        .query_fenced(
            [
                CheckedStatement {
                    sql: "SELECT streamer_id FROM relay.users WHERE streamer_id=$1 AND enabled=true AND $2::text IS NOT NULL AND $3::text IN ('pov','camera') AND char_length($4::text)=64 AND octet_length($5::bytea)>0 FOR UPDATE",
                    expected_rows: None,
                },
                CheckedStatement {
                    sql: r#"
                    WITH source AS (
                        INSERT INTO relay.cast_sources(streamer_id,label,kind,ingest_key_hash,ingest_key_enc)
                        SELECT $1,$2,$3,$4,$5
                        WHERE EXISTS(SELECT 1 FROM relay.users WHERE streamer_id=$1 AND enabled=true)
                          AND (SELECT count(*) FROM relay.cast_sources WHERE streamer_id=$1) < 8
                        RETURNING source_id
                    ), state_row AS (
                        INSERT INTO relay.cast_state(streamer_id)
                        SELECT $1 FROM source
                        ON CONFLICT(streamer_id) DO NOTHING
                        RETURNING streamer_id
                    ), scene AS (
                        INSERT INTO relay.cast_scenes(streamer_id,name,source_id,sort_order)
                        SELECT $1,$2,source_id,COALESCE((SELECT max(sort_order)+10 FROM relay.cast_scenes WHERE streamer_id=$1),0)
                        FROM source
                        RETURNING scene_id,source_id
                    )
                    SELECT scene.source_id,scene.scene_id FROM scene
                    "#,
                    expected_rows: None,
                },
            ],
            &[&request.streamer_id, &label, &request.kind, &hash, &encrypted],
            None,
        )
        .await
        .map_err(|error| failure(StatusCode::SERVICE_UNAVAILABLE, error))?;
    let row = rows.first().ok_or_else(|| {
        failure(
            StatusCode::CONFLICT,
            "Quellenlimit ist erreicht oder der Zugang ist nicht freigeschaltet.",
        )
    })?;
    let source_id: i64 = row.try_get(0).map_err(|_| invalid_cast_data())?;
    let scene_id: i64 = row.try_get(1).map_err(|_| invalid_cast_data())?;
    Ok(Json(json!({
        "source_id": source_id,
        "scene_id": scene_id,
        "label": label,
        "kind": request.kind,
        "ingest_key": key.as_str(),
        "ingest_url": state.config.public_ingest_url,
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceMutation {
    streamer_id: i64,
}

async fn rotate_source(
    State(state): State<Arc<ServiceState>>,
    headers: HeaderMap,
    Path(source_id): Path<u64>,
    payload: Result<Json<SourceMutation>, axum::extract::rejection::JsonRejection>,
) -> ApiResult {
    let Json(request) = payload.map_err(|_| {
        failure(
            StatusCode::BAD_REQUEST,
            "Anfrage enthält ungültige JSON-Daten.",
        )
    })?;
    authorize(&state, &headers, request.streamer_id)?;
    let tenant = u64::try_from(request.streamer_id)
        .map_err(|_| failure(StatusCode::BAD_REQUEST, "Nutzeridentität ist ungültig."))?;
    if source_id == 0 || state.registry.source_active(tenant, source_id) {
        return Err(failure(
            StatusCode::CONFLICT,
            "Quelle sendet gerade. Beende diesen Eingang vor dem Schlüsselwechsel.",
        ));
    }
    let source_i64 = i64::try_from(source_id)
        .map_err(|_| failure(StatusCode::BAD_REQUEST, "Quellenidentität ist ungültig."))?;
    let key = source_key().map_err(|error| failure(StatusCode::SERVICE_UNAVAILABLE, error))?;
    let hash = key_hash(&key);
    let encrypted = state
        .secrets
        .encryption
        .seal(key.as_bytes(), &source_binding(request.streamer_id, &hash))
        .map_err(|error| failure(StatusCode::SERVICE_UNAVAILABLE, error))?;
    let rows = state
        .store
        .query(
            "UPDATE relay.cast_sources SET ingest_key_hash=$3,ingest_key_enc=$4,updated_at=now() WHERE streamer_id=$1 AND source_id=$2 RETURNING source_id",
            &[&request.streamer_id, &source_i64, &hash, &encrypted],
        )
        .await
        .map_err(|error| failure(StatusCode::SERVICE_UNAVAILABLE, error))?;
    if rows.is_empty() {
        return Err(failure(
            StatusCode::NOT_FOUND,
            "Quelle wurde nicht gefunden.",
        ));
    }
    Ok(Json(
        json!({"source_id": source_id, "ingest_key": key.as_str()}),
    ))
}

async fn delete_source(
    State(state): State<Arc<ServiceState>>,
    headers: HeaderMap,
    Path(source_id): Path<u64>,
    Query(query): Query<TenantQuery>,
) -> ApiResult {
    authorize(&state, &headers, query.streamer_id)?;
    let tenant = u64::try_from(query.streamer_id)
        .map_err(|_| failure(StatusCode::BAD_REQUEST, "Nutzeridentität ist ungültig."))?;
    if source_id == 0 || state.registry.source_active(tenant, source_id) {
        return Err(failure(
            StatusCode::CONFLICT,
            "Quelle sendet gerade und kann deshalb nicht gelöscht werden.",
        ));
    }
    let source_i64 = i64::try_from(source_id)
        .map_err(|_| failure(StatusCode::BAD_REQUEST, "Quellenidentität ist ungültig."))?;
    let rows = state
        .store
        .query(
            r#"
            WITH affected AS (
                SELECT scene_id FROM relay.cast_scenes WHERE streamer_id=$1 AND source_id=$2
            ), bumped AS (
                UPDATE relay.cast_state
                SET program_scene_id=CASE
                      WHEN program_scene_id IN (SELECT scene_id FROM affected) THEN NULL
                      ELSE program_scene_id
                    END,
                    preview_scene_id=CASE
                      WHEN preview_scene_id IN (SELECT scene_id FROM affected) THEN NULL
                      ELSE preview_scene_id
                    END,
                    switch_generation=switch_generation+1,
                    updated_at=now()
                WHERE streamer_id=$1
                  AND (program_scene_id IN (SELECT scene_id FROM affected)
                    OR preview_scene_id IN (SELECT scene_id FROM affected))
                RETURNING streamer_id
            ), deleted_scenes AS (
                DELETE FROM relay.cast_scenes
                WHERE streamer_id=$1 AND source_id=$2
                RETURNING scene_id
            ), deleted_source AS (
                DELETE FROM relay.cast_sources
                WHERE streamer_id=$1 AND source_id=$2
                  AND (SELECT count(*) FROM deleted_scenes) >= 0
                RETURNING source_id
            )
            SELECT source_id FROM deleted_source
            "#,
            &[&query.streamer_id, &source_i64],
        )
        .await
        .map_err(|error| failure(StatusCode::SERVICE_UNAVAILABLE, error))?;
    if rows.is_empty() {
        return Err(failure(
            StatusCode::NOT_FOUND,
            "Quelle wurde nicht gefunden.",
        ));
    }
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateScene {
    streamer_id: i64,
    name: String,
    source_id: u64,
}

async fn create_scene(
    State(state): State<Arc<ServiceState>>,
    headers: HeaderMap,
    payload: Result<Json<CreateScene>, axum::extract::rejection::JsonRejection>,
) -> ApiResult {
    let Json(request) = payload.map_err(|_| {
        failure(
            StatusCode::BAD_REQUEST,
            "Anfrage enthält ungültige JSON-Daten.",
        )
    })?;
    authorize(&state, &headers, request.streamer_id)?;
    let name = valid_name(&request.name)
        .ok_or_else(|| failure(StatusCode::BAD_REQUEST, "Szenenname ist ungültig."))?;
    let source_id = i64::try_from(request.source_id)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| failure(StatusCode::BAD_REQUEST, "Quellenidentität ist ungültig."))?;
    let rows = state
        .store
        .query_fenced(
            [
                CheckedStatement {
                    sql: "SELECT streamer_id FROM relay.users WHERE streamer_id=$1 AND $2::text IS NOT NULL AND $3::bigint>0 FOR UPDATE",
                    expected_rows: None,
                },
                CheckedStatement {
                    sql: r#"
                    INSERT INTO relay.cast_scenes(streamer_id,name,source_id,sort_order)
                    SELECT $1,$2,$3,COALESCE((SELECT max(sort_order)+10 FROM relay.cast_scenes WHERE streamer_id=$1),0)
                    WHERE EXISTS(SELECT 1 FROM relay.cast_sources WHERE streamer_id=$1 AND source_id=$3 AND enabled=true)
                      AND (SELECT count(*) FROM relay.cast_scenes WHERE streamer_id=$1) < 24
                    RETURNING scene_id
                    "#,
                    expected_rows: None,
                },
            ],
            &[&request.streamer_id, &name, &source_id],
            None,
        )
        .await
        .map_err(|error| failure(StatusCode::SERVICE_UNAVAILABLE, error))?;
    let scene_id: i64 = rows
        .first()
        .ok_or_else(|| {
            failure(
                StatusCode::CONFLICT,
                "Quelle fehlt oder das Szenenlimit ist erreicht.",
            )
        })?
        .try_get(0)
        .map_err(|_| invalid_cast_data())?;
    Ok(Json(
        json!({"scene_id": scene_id, "name": name, "source_id": request.source_id}),
    ))
}

async fn delete_scene(
    State(state): State<Arc<ServiceState>>,
    headers: HeaderMap,
    Path(scene_id): Path<u64>,
    Query(query): Query<TenantQuery>,
) -> ApiResult {
    authorize(&state, &headers, query.streamer_id)?;
    let scene_id = i64::try_from(scene_id)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| failure(StatusCode::BAD_REQUEST, "Szenenidentität ist ungültig."))?;
    let rows = state
        .store
        .query(
            r#"
            WITH bumped AS (
                UPDATE relay.cast_state
                SET program_scene_id=CASE WHEN program_scene_id=$2 THEN NULL ELSE program_scene_id END,
                    preview_scene_id=CASE WHEN preview_scene_id=$2 THEN NULL ELSE preview_scene_id END,
                    switch_generation=switch_generation+1,
                    updated_at=now()
                WHERE streamer_id=$1 AND (program_scene_id=$2 OR preview_scene_id=$2)
                RETURNING streamer_id
            ), deleted AS (
                DELETE FROM relay.cast_scenes
                WHERE streamer_id=$1 AND scene_id=$2
                  AND (SELECT count(*) FROM bumped) >= 0
                RETURNING scene_id
            )
            SELECT scene_id FROM deleted
            "#,
            &[&query.streamer_id, &scene_id],
        )
        .await
        .map_err(|error| failure(StatusCode::SERVICE_UNAVAILABLE, error))?;
    if rows.is_empty() {
        return Err(failure(
            StatusCode::NOT_FOUND,
            "Szene wurde nicht gefunden.",
        ));
    }
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviewSelection {
    streamer_id: i64,
    scene_id: Option<u64>,
    expected_generation: i64,
}

async fn select_preview(
    State(state): State<Arc<ServiceState>>,
    headers: HeaderMap,
    payload: Result<Json<PreviewSelection>, axum::extract::rejection::JsonRejection>,
) -> ApiResult {
    let Json(request) = payload.map_err(|_| {
        failure(
            StatusCode::BAD_REQUEST,
            "Anfrage enthält ungültige JSON-Daten.",
        )
    })?;
    authorize(&state, &headers, request.streamer_id)?;
    if request.expected_generation < 0 {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "Schaltgeneration ist ungültig.",
        ));
    }
    let scene_id = request
        .scene_id
        .map(i64::try_from)
        .transpose()
        .ok()
        .flatten();
    if request.scene_id.is_some() && scene_id.is_none() {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "Szenenidentität ist ungültig.",
        ));
    }
    let rows = state
        .store
        .query(
            r#"
            WITH changed AS (
                UPDATE relay.cast_state s
                SET preview_scene_id=$2,switch_generation=switch_generation+1,updated_at=now()
                WHERE streamer_id=$1 AND switch_generation=$3
                  AND ($2::bigint IS NULL OR EXISTS(
                    SELECT 1 FROM relay.cast_scenes c WHERE c.streamer_id=$1 AND c.scene_id=$2
                  ))
                RETURNING switch_generation,program_scene_id,preview_scene_id
            )
            SELECT switch_generation,program_scene_id,preview_scene_id,
              (SELECT source_id FROM relay.cast_scenes WHERE streamer_id=$1 AND scene_id=program_scene_id),
              (SELECT source_id FROM relay.cast_scenes WHERE streamer_id=$1 AND scene_id=preview_scene_id)
            FROM changed
            "#,
            &[&request.streamer_id, &scene_id, &request.expected_generation],
        )
        .await
        .map_err(|error| failure(StatusCode::SERVICE_UNAVAILABLE, error))?;
    selection_result(&state, request.streamer_id, rows)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgramSelection {
    streamer_id: i64,
    scene_id: u64,
    expected_generation: i64,
    #[serde(default = "default_swap")]
    swap_preview: bool,
}

const fn default_swap() -> bool {
    true
}

async fn select_program(
    State(state): State<Arc<ServiceState>>,
    headers: HeaderMap,
    payload: Result<Json<ProgramSelection>, axum::extract::rejection::JsonRejection>,
) -> ApiResult {
    let Json(request) = payload.map_err(|_| {
        failure(
            StatusCode::BAD_REQUEST,
            "Anfrage enthält ungültige JSON-Daten.",
        )
    })?;
    authorize(&state, &headers, request.streamer_id)?;
    let scene_id = i64::try_from(request.scene_id)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| failure(StatusCode::BAD_REQUEST, "Szenenidentität ist ungültig."))?;
    if request.expected_generation < 0 {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "Schaltgeneration ist ungültig.",
        ));
    }
    let tenant = u64::try_from(request.streamer_id)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| failure(StatusCode::BAD_REQUEST, "Nutzeridentität ist ungültig."))?;
    let scene_source = state
        .store
        .query(
            "SELECT source_id FROM relay.cast_scenes WHERE streamer_id=$1 AND scene_id=$2",
            &[&request.streamer_id, &scene_id],
        )
        .await
        .map_err(|error| failure(StatusCode::SERVICE_UNAVAILABLE, error))?;
    let source_id: i64 = scene_source
        .first()
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "Szene wurde nicht gefunden."))?
        .try_get(0)
        .map_err(|_| invalid_cast_data())?;
    let source_id = u64::try_from(source_id)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(invalid_cast_data)?;
    if !state.registry.source_active(tenant, source_id) {
        return Err(failure(
            StatusCode::CONFLICT,
            "Die gewählte Program-Quelle ist nicht verbunden.",
        ));
    }
    let rows = state
        .store
        .query(
            r#"
            WITH changed AS (
                UPDATE relay.cast_state s
                SET program_scene_id=$2,
                    preview_scene_id=CASE WHEN $4 THEN program_scene_id ELSE preview_scene_id END,
                    switch_generation=switch_generation+1,
                    updated_at=now()
                WHERE streamer_id=$1 AND switch_generation=$3
                  AND EXISTS(SELECT 1 FROM relay.cast_scenes c WHERE c.streamer_id=$1 AND c.scene_id=$2)
                RETURNING switch_generation,program_scene_id,preview_scene_id
            )
            SELECT switch_generation,program_scene_id,preview_scene_id,
              (SELECT source_id FROM relay.cast_scenes WHERE streamer_id=$1 AND scene_id=program_scene_id),
              (SELECT source_id FROM relay.cast_scenes WHERE streamer_id=$1 AND scene_id=preview_scene_id)
            FROM changed
            "#,
            &[
                &request.streamer_id,
                &scene_id,
                &request.expected_generation,
                &request.swap_preview,
            ],
        )
        .await
        .map_err(|error| failure(StatusCode::SERVICE_UNAVAILABLE, error))?;
    selection_result(&state, request.streamer_id, rows)
}

fn selection_result(
    state: &ServiceState,
    streamer_id: i64,
    rows: Vec<tokio_postgres::Row>,
) -> ApiResult {
    let row = rows.first().ok_or_else(|| {
        failure(
            StatusCode::CONFLICT,
            "Studio wurde zwischenzeitlich geändert oder die Szene existiert nicht mehr. Aktuellen Stand neu laden.",
        )
    })?;
    let generation: i64 = row.try_get(0).map_err(|_| invalid_cast_data())?;
    let program_scene_id: Option<i64> = row.try_get(1).map_err(|_| invalid_cast_data())?;
    let preview_scene_id: Option<i64> = row.try_get(2).map_err(|_| invalid_cast_data())?;
    let program_source: Option<i64> = row.try_get(3).map_err(|_| invalid_cast_data())?;
    let preview_source: Option<i64> = row.try_get(4).map_err(|_| invalid_cast_data())?;
    let tenant = u64::try_from(streamer_id).map_err(|_| invalid_cast_data())?;
    state.registry.set_cast_selection(
        tenant,
        program_source.and_then(|value| u64::try_from(value).ok()),
        preview_source.and_then(|value| u64::try_from(value).ok()),
    );
    Ok(Json(json!({
        "switch_generation": generation,
        "program_scene_id": program_scene_id,
        "preview_scene_id": preview_scene_id,
    })))
}

pub(crate) async fn selected_source(
    state: &ServiceState,
    streamer_id: i64,
) -> Result<(Option<u64>, Option<u64>, i64), &'static str> {
    let rows = state
        .store
        .query(
            r#"
            SELECT
              (SELECT source_id FROM relay.cast_scenes WHERE streamer_id=$1 AND scene_id=s.program_scene_id),
              (SELECT source_id FROM relay.cast_scenes WHERE streamer_id=$1 AND scene_id=s.preview_scene_id),
              s.switch_generation
            FROM relay.cast_state s WHERE s.streamer_id=$1
            "#,
            &[&streamer_id],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok((None, None, 0));
    };
    let program = row
        .try_get::<_, Option<i64>>(0)
        .map_err(|_| "Cast-Studio-Zustand ist ungültig.")?
        .and_then(|value| u64::try_from(value).ok());
    let preview = row
        .try_get::<_, Option<i64>>(1)
        .map_err(|_| "Cast-Studio-Zustand ist ungültig.")?
        .and_then(|value| u64::try_from(value).ok());
    let generation = row
        .try_get::<_, i64>(2)
        .map_err(|_| "Cast-Studio-Zustand ist ungültig.")?;
    Ok((program, preview, generation))
}
