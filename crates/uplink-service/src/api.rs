use crate::{config::Config, registry::Registry, secrets::ServiceSecrets, store::Store};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{delete, get, post, put},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

pub struct ServiceState {
    pub chat: Option<Arc<uplink_chat::ChatHub>>,
    pub config: Config,
    pub store: Arc<Store>,
    pub secrets: Arc<ServiceSecrets>,
    pub registry: Registry,
    pub tls: Option<Arc<crate::tls_reload::ReloadingTls>>,
}
#[derive(Deserialize)]
pub struct TenantQuery {
    pub streamer_id: i64,
}
pub type ApiResult = Result<Json<Value>, (StatusCode, Json<Value>)>;
fn failure(status: StatusCode, message: &'static str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"error": message})))
}
fn authorize(
    state: &ServiceState,
    headers: &HeaderMap,
    tenant: i64,
) -> Result<(), (StatusCode, Json<Value>)> {
    let candidate = headers
        .get("X-Relay-Auth")
        .map_or(&[][..], |v| v.as_bytes());
    if state.secrets.api.expose().is_empty()
        || !state.secrets.api.matches(candidate)
        || tenant <= 0
        || !state.config.permits_tenant(tenant as u64)
    {
        return Err(failure(
            StatusCode::UNAUTHORIZED,
            "Zugriff wurde abgewiesen.",
        ));
    }
    Ok(())
}
pub fn router(state: Arc<ServiceState>) -> Router {
    let timeout_seconds = state.config.request_timeout_seconds;
    let slots = Arc::new(tokio::sync::Semaphore::new(32));
    let routes = Router::new()
        .route("/v1/health", get(health))
        .route("/v1/me", get(me))
        .route("/v1/me/status", get(status));
    let routes = if state.config.test_ingest.is_some() {
        routes.route("/v1/me/destinations", get(destinations))
    } else {
        routes
            .route(
                "/v1/me/destinations",
                get(destinations).put(save_destinations),
            )
            .route("/v1/me/waitlist", post(waitlist))
            .route("/v1/me/key/rotate", post(rotate_key))
            .route("/v1/me/dock/rotate", post(rotate_dock))
            .route("/v1/me/dock-token/rotate", post(rotate_dock))
            .route("/v1/me/reconnect-wait", put(reconnect_wait))
            .route(
                "/v1/me/twitch/native-2k-hardware",
                get(native_2k_hardware)
                    .put(save_native_2k_hardware)
                    .delete(delete_native_2k_hardware),
            )
            .route("/v1/me/destinations/{platform}", delete(delete_destination))
            .route("/v1/caps", get(caps))
            .route("/v1/admin/waitlist", get(admin_waitlist))
            .route("/v1/admin/waitlist/{id}", delete(reject_waitlist))
            .route("/v1/admin/users", post(admit_user))
    };
    let routes = routes.with_state(state.clone());
    let routes = if let Some(hub) = &state.chat {
        routes.merge(uplink_chat::router(hub.clone()))
    } else {
        routes
    };
    routes
        .layer(DefaultBodyLimit::max(32 * 1024))
        .layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let slots = slots.clone();
                async move {
                    use axum::response::IntoResponse;
                    let Ok(_permit) = slots.try_acquire_owned() else {
                        return failure(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "Steuerung ist ausgelastet.",
                        )
                        .into_response();
                    };
                    let deadline = tokio::time::Instant::now()
                        + std::time::Duration::from_secs(timeout_seconds);
                    let mut response = match tokio::time::timeout_at(
                        deadline + crate::store::CLEANUP_GRACE,
                        crate::store::REQUEST_DEADLINE.scope(deadline, next.run(request)),
                    )
                    .await
                    {
                        Ok(response) => response,
                        Err(_) => failure(
                            StatusCode::GATEWAY_TIMEOUT,
                            "Anfragefrist überschritten; Abschluss unklar, gespeicherten Stand vor Wiederholung prüfen.",
                        )
                        .into_response(),
                    };
                    response.headers_mut().insert(
                        axum::http::header::CACHE_CONTROL,
                        axum::http::HeaderValue::from_static("no-store"),
                    );
                    response.headers_mut().insert(
                        axum::http::header::REFERRER_POLICY,
                        axum::http::HeaderValue::from_static("no-referrer"),
                    );
                    response
                }
            },
        ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DestinationRequest {
    streamer_id: i64,
    destinations: Vec<crate::destinations::DestinationUpdate>,
}
async fn save_destinations(
    State(state): State<Arc<ServiceState>>,
    headers: HeaderMap,
    payload: Result<Json<DestinationRequest>, axum::extract::rejection::JsonRejection>,
) -> ApiResult {
    let Json(request) = payload.map_err(|_| {
        failure(
            StatusCode::BAD_REQUEST,
            "Anfrage enthält ungültige JSON-Daten.",
        )
    })?;
    authorize(&state, &headers, request.streamer_id)?;
    if request.destinations.is_empty() || request.destinations.len() > 4 {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "Die Anzahl der Ziele ist ungültig.",
        ));
    }
    let mut ids = std::collections::HashSet::new();
    let mut rows = Vec::new();
    for output in &request.destinations {
        output
            .validate()
            .map_err(|e| failure(StatusCode::BAD_REQUEST, e))?;
        output
            .validate_policy(&state.config)
            .map_err(|e| failure(StatusCode::BAD_REQUEST, e))?;
        if !ids.insert(&output.platform) {
            return Err(failure(
                StatusCode::BAD_REQUEST,
                "Ziel ist mehrfach enthalten.",
            ));
        }
        let ciphertext = output
            .stream_key
            .as_ref()
            .filter(|key| !key.is_empty())
            .map(|key| {
                state.secrets.encryption.seal(
                    key.as_bytes(),
                    &format!("destination:{}:{}", request.streamer_id, output.platform),
                )
            })
            .transpose()
            .map_err(|e| failure(StatusCode::SERVICE_UNAVAILABLE, e))?;
        // twitch_audio_mode ist nur noch ein Legacy-Feld für rollende Clients.
        // Ein alter Client darf damit die automatische Live/VOD-Trennung nicht
        // wieder abschalten. NULL sorgt dafür, dass der bestehende DB-Wert bei
        // Teilupdates unverändert bleibt; der Medienpfad ignoriert ihn ohnehin.
        rows.push(json!({"platform":output.platform,"connection_generation":output.connection_generation,"rtmp_url":output.rtmp_url,"stream_key_enc":ciphertext.map(|bytes|format!("\\x{}",hex::encode(bytes))),"enabled":output.enabled,"width":output.width,"height":output.height,"fps":output.fps,"bitrate_kbps":output.bitrate_kbps,"twitch_audio_mode":serde_json::Value::Null,"twitch_output_mode":output.twitch_output_mode}));
    }
    let data = json!(rows);
    // INSERT benötigt Pflichtwerte auch für bestehende Ziele. Beim Konflikt
    // zählen für ausgelassene Felder jedoch die jetzt gesperrten aktuellen
    // Werte, niemals die vorher gelesenen INSERT-Snapshotwerte aus EXCLUDED.
    let fence_sql = r#"
        WITH incoming AS (
            SELECT * FROM jsonb_to_recordset($2::jsonb) AS x(
                platform text, rtmp_url text, stream_key_enc bytea,
                enabled boolean, width integer, height integer,
                fps integer, bitrate_kbps integer, connection_generation bigint, twitch_audio_mode text, twitch_output_mode text
            )
        )
            INSERT INTO relay.destination_fences AS current(streamer_id,platform,generation,deleted)
            SELECT $1,platform,connection_generation,false FROM incoming
            WHERE EXISTS(SELECT 1 FROM relay.users WHERE streamer_id=$1 AND enabled=true)
            ORDER BY platform COLLATE "C"
            ON CONFLICT(streamer_id,platform) DO UPDATE SET
                generation=EXCLUDED.generation,deleted=false
            WHERE EXCLUDED.generation > current.generation
                OR (EXCLUDED.generation=current.generation AND NOT current.deleted)
            RETURNING platform
    "#;
    let write_sql = r#"
        WITH incoming AS (
            SELECT * FROM jsonb_to_recordset($2::jsonb) AS x(
                platform text, rtmp_url text, stream_key_enc bytea,
                enabled boolean, width integer, height integer,
                fps integer, bitrate_kbps integer, connection_generation bigint, twitch_audio_mode text, twitch_output_mode text
            )
        )
        INSERT INTO relay.destinations(
            streamer_id, platform, rtmp_url, stream_key_enc,
            enabled, width, height, fps, bitrate_kbps, twitch_audio_mode, twitch_output_mode
        )
        SELECT $1, i.platform, COALESCE(i.rtmp_url,p.rtmp_url),
            COALESCE(i.stream_key_enc,p.stream_key_enc),
            COALESCE(i.enabled,p.enabled,true), COALESCE(i.width,p.width),
            COALESCE(i.height,p.height), COALESCE(i.fps,p.fps),
            COALESCE(i.bitrate_kbps,p.bitrate_kbps), COALESCE(i.twitch_audio_mode,p.twitch_audio_mode),
            COALESCE(i.twitch_output_mode,p.twitch_output_mode,'single')
        FROM incoming i
        LEFT JOIN relay.destinations p
            ON p.streamer_id=$1 AND p.platform=i.platform
        WHERE EXISTS(SELECT 1 FROM relay.users WHERE streamer_id=$1 AND enabled=true)
        ORDER BY i.platform COLLATE "C"
        ON CONFLICT(streamer_id,platform) DO UPDATE SET
            rtmp_url=COALESCE((SELECT rtmp_url FROM incoming WHERE platform=EXCLUDED.platform),relay.destinations.rtmp_url),
            stream_key_enc=COALESCE((SELECT stream_key_enc FROM incoming WHERE platform=EXCLUDED.platform),relay.destinations.stream_key_enc),
            enabled=COALESCE((SELECT enabled FROM incoming WHERE platform=EXCLUDED.platform),relay.destinations.enabled),
            width=COALESCE((SELECT width FROM incoming WHERE platform=EXCLUDED.platform),relay.destinations.width),
            height=COALESCE((SELECT height FROM incoming WHERE platform=EXCLUDED.platform),relay.destinations.height),
            fps=COALESCE((SELECT fps FROM incoming WHERE platform=EXCLUDED.platform),relay.destinations.fps),
            bitrate_kbps=COALESCE((SELECT bitrate_kbps FROM incoming WHERE platform=EXCLUDED.platform),relay.destinations.bitrate_kbps),
            twitch_audio_mode=COALESCE((SELECT twitch_audio_mode FROM incoming WHERE platform=EXCLUDED.platform),relay.destinations.twitch_audio_mode),
            twitch_output_mode=COALESCE((SELECT twitch_output_mode FROM incoming WHERE platform=EXCLUDED.platform),relay.destinations.twitch_output_mode)
        RETURNING platform
    "#;
    state
        .store
        .query_fenced(
            [
                crate::store::CheckedStatement {
                    sql: fence_sql,
                    expected_rows: Some(request.destinations.len()),
                },
                crate::store::CheckedStatement {
                    sql: write_sql,
                    expected_rows: Some(request.destinations.len()),
                },
            ],
            &[&request.streamer_id, &data],
            None,
        )
        .await
        .map_err(|error| {
            failure(
                if error.starts_with("Zielgeneration") {
                    StatusCode::CONFLICT
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                },
                error,
            )
        })?;
    let generations: std::collections::BTreeMap<_, _> = request
        .destinations
        .iter()
        .map(|d| (&d.platform, d.connection_generation))
        .collect();
    Ok(Json(
        json!({"ok":true,"connection_generations":generations,"live_quality":{"status":"next_stream","message":"Das Wunschprofil ist gespeichert und wird beim nächsten Stream anhand des Eingangs geprüft."}}),
    ))
}

async fn waitlist(
    State(state): State<Arc<ServiceState>>,
    headers: HeaderMap,
    Query(query): Query<TenantQuery>,
) -> ApiResult {
    authorize(&state, &headers, query.streamer_id)?;
    state.store.query("INSERT INTO relay.waitlist(streamer_id) VALUES($1) ON CONFLICT(streamer_id) DO NOTHING",&[&query.streamer_id]).await.map_err(|e|failure(StatusCode::SERVICE_UNAVAILABLE,e))?;
    Ok(Json(json!({"ok":true})))
}
async fn rotate_key(
    State(state): State<Arc<ServiceState>>,
    headers: HeaderMap,
    Query(query): Query<TenantQuery>,
) -> ApiResult {
    authorize(&state, &headers, query.streamer_id)?;
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(|_| {
        failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "Zufallsquelle ist nicht verfügbar.",
        )
    })?;
    let key = zeroize::Zeroizing::new(format!("rsr_{}", hex::encode(bytes)));
    let sealed = state
        .secrets
        .encryption
        .seal(key.as_bytes(), &format!("ingest_key:{}", query.streamer_id))
        .map_err(|e| failure(StatusCode::SERVICE_UNAVAILABLE, e))?;
    use sha2::Digest;
    let hash = hex::encode(sha2::Sha256::digest(key.as_bytes()));
    let rows=state.store.query("UPDATE relay.users SET ingest_key_hash=$2,ingest_key_enc=$3 WHERE streamer_id=$1 AND enabled=true RETURNING streamer_id",&[&query.streamer_id,&hash,&sealed]).await.map_err(|e|failure(StatusCode::SERVICE_UNAVAILABLE,e))?;
    if rows.is_empty() {
        return Err(failure(
            StatusCode::FORBIDDEN,
            "Der Zugang ist nicht freigeschaltet.",
        ));
    }
    Ok(Json(
        json!({"ingest_key":key.as_str(),"ingest_url":state.config.public_ingest_url}),
    ))
}
async fn health(State(state): State<Arc<ServiceState>>) -> (StatusCode, Json<Value>) {
    let database_ready = state.store.ready().await;
    let tls_ready = state.tls.as_ref().is_none_or(|tls| tls.ready());
    let ready = database_ready && tls_ready;
    (
        if ready {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(
            json!({"ok":ready,"service":"uplink", "active_sessions":state.registry.active_count(),
                "database_ready":database_ready,"tls_ready":tls_ready,
                "tls_refresh_failed":state.tls.as_ref().is_some_and(|tls|tls.refresh_failed()),
                "ingest_test":state.config.test_ingest.is_some()}),
        ),
    )
}
async fn rotate_dock(
    State(state): State<Arc<ServiceState>>,
    headers: HeaderMap,
    Query(query): Query<TenantQuery>,
) -> ApiResult {
    authorize(&state, &headers, query.streamer_id)?;
    let mut random = [0; 16];
    getrandom::fill(&mut random).map_err(|_| {
        failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "Zufallsquelle ist nicht verfügbar.",
        )
    })?;
    let token = zeroize::Zeroizing::new(format!("dock_{}", hex::encode(random)));
    use sha2::Digest;
    let hash = hex::encode(sha2::Sha256::digest(token.as_bytes()));
    let encrypted = state
        .secrets
        .encryption
        .seal(
            token.as_bytes(),
            &format!("dock_token:{}", query.streamer_id),
        )
        .map_err(|e| failure(StatusCode::SERVICE_UNAVAILABLE, e))?;
    let rows = state.store.query("UPDATE relay.users SET dock_token_hash=$2,dock_token_enc=$3 WHERE streamer_id=$1 AND enabled=true RETURNING streamer_id", &[&query.streamer_id,&hash,&encrypted]).await
        .map_err(|e|failure(StatusCode::SERVICE_UNAVAILABLE,e))?;
    if rows.is_empty() {
        return Err(failure(
            StatusCode::FORBIDDEN,
            "Der Zugang ist nicht freigeschaltet.",
        ));
    }
    if let Some(hub) = &state.chat {
        hub.rotate_docks(query.streamer_id as u64);
    }
    Ok(Json(
        json!({"ok":true,"dock_urls":dock_urls(&state.config.dock_base_url,&token)}),
    ))
}
fn dock_urls(base: &str, token: &str) -> Value {
    let base = base.trim_end_matches('/');
    json!({"chat":format!("{base}/dock/chat?t={token}"),"activity":format!("{base}/dock/activity?t={token}"),"stream_info":format!("{base}/dock/stream-info?t={token}"),"points":format!("{base}/dock/points?t={token}")})
}
fn service_status(state: &ServiceState) -> &'static str {
    if state.config.test_ingest.is_some() {
        "input_only"
    } else if state.tls.as_ref().is_some_and(|tls| !tls.ready()) {
        "unavailable"
    } else {
        "ready"
    }
}
fn capabilities() -> Value {
    json!({"reconnect":false,"layout":false,"delay":false,"vod":false})
}
fn dashboard_state(state: &ServiceState, id: u64) -> Value {
    let statuses = state.registry.status(id);
    let session = statuses
        .iter()
        .find(|s| s.active)
        .map(crate::media_status::session_status);
    let statuses: Vec<_> = statuses
        .iter()
        .map(crate::media_status::session_status)
        .collect();
    json!({"sessions":statuses,"session":session,"service_status":service_status(state),"capabilities":capabilities()})
}
async fn status(
    State(state): State<Arc<ServiceState>>,
    headers: HeaderMap,
    Query(query): Query<TenantQuery>,
) -> ApiResult {
    authorize(&state, &headers, query.streamer_id)?;
    let mut status = dashboard_state(&state, query.streamer_id as u64);
    let database_ready = state.store.ready().await;
    status["database_ready"] = json!(database_ready);
    if !database_ready {
        status["service_status"] = json!("unavailable");
    }
    Ok(Json(status))
}
async fn me(
    State(state): State<Arc<ServiceState>>,
    headers: HeaderMap,
    Query(query): Query<TenantQuery>,
) -> ApiResult {
    authorize(&state, &headers, query.streamer_id)?;
    let rows = state.store.query("SELECT u.enabled,u.ingest_key_enc,u.dock_token_enc,u.reconnect_wait_s,EXISTS(SELECT 1 FROM relay.waitlist WHERE streamer_id=$1) AS waitlisted,u.ingest_key_hash FROM (SELECT $1::bigint AS streamer_id) requested LEFT JOIN relay.users u USING(streamer_id)", &[&query.streamer_id]).await.map_err(|e| failure(StatusCode::SERVICE_UNAVAILABLE,e))?;
    let row = rows.first().ok_or_else(|| {
        failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "Nutzerstatus ist nicht verfügbar.",
        )
    })?;
    let waitlisted: bool = row.try_get(4).map_err(|_| {
        failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "Wartelistenstatus ist ungültig.",
        )
    })?;
    let enabled: Option<bool> = row.try_get(0).map_err(|_| {
        failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "Nutzerstatus ist ungültig.",
        )
    })?;
    let Some(enabled) = enabled else {
        return Ok(Json(
            json!({"enabled":false,"waitlisted":waitlisted,"ingest_key":"","public_ingest_url":state.config.public_ingest_url,"ingest_url":state.config.public_ingest_url,"service_status":service_status(&state),"capabilities":capabilities(),"srt_hint":"","session":null,"public_visible":false,"status_text":"Zugang ist noch nicht freigeschaltet.","reconnect_wait_s":0,"dock_url_vorhanden":false,"dock_urls":null,"chat":[]}),
        ));
    };
    let encrypted: Option<Vec<u8>> = row.try_get(1).map_err(|_| {
        failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Streamzugang ist nicht verfügbar.",
        )
    })?;
    let key = encrypted
        .filter(|_| enabled)
        .as_ref()
        .map(|encrypted| {
            state
                .secrets
                .encryption
                .open(encrypted, &format!("ingest_key:{}", query.streamer_id))
        })
        .transpose()
        .map_err(|e| failure(StatusCode::SERVICE_UNAVAILABLE, e))?;
    if enabled {
        let expected: Option<String> = row.try_get(5).map_err(|_| invalid_credentials())?;
        let actual = credential_hash(key.as_ref().ok_or_else(invalid_credentials)?.expose())?;
        if expected.as_ref() != Some(&actual) {
            return Err(invalid_credentials());
        }
    }
    let key = std::str::from_utf8(key.as_ref().map_or(&[][..], |value| value.expose())).map_err(
        |_| {
            failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Streamzugang ist ungültig.",
            )
        },
    )?;
    let dock: Option<Vec<u8>> = row.try_get(2).map_err(|_| {
        failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Dockzugang ist ungültig.",
        )
    })?;
    let dock_urls = if let Some(dock) = dock.filter(|_| enabled) {
        let token = state
            .secrets
            .encryption
            .open(&dock, &format!("dock_token:{}", query.streamer_id))
            .map_err(|e| failure(StatusCode::SERVICE_UNAVAILABLE, e))?;
        let token = std::str::from_utf8(token.expose()).map_err(|_| {
            failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Dockzugang ist ungültig.",
            )
        })?;
        if token.len() != 37
            || !token.starts_with("dock_")
            || !token[5..].bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Dockzugang ist ungültig.",
            ));
        }
        dock_urls(&state.config.dock_base_url, token)
    } else {
        Value::Null
    };
    let wait: i32 = row.try_get(3).map_err(|_| {
        failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Nutzerdaten sind ungültig.",
        )
    })?;
    let chat = match &state.chat {
        Some(hub) if enabled => serde_json::to_value(
            hub.status_for(query.streamer_id as u64)
                .map_err(|e| failure(StatusCode::SERVICE_UNAVAILABLE, e))?,
        )
        .map_err(|_| {
            failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "Chatstatus ist nicht verfügbar.",
            )
        })?,
        Some(hub) => {
            hub.invalidate(query.streamer_id as u64);
            json!([])
        }
        None => json!([]),
    };
    let status = dashboard_state(&state, query.streamer_id as u64);
    Ok(Json(
        json!({"enabled":enabled,"waitlisted":waitlisted,"ingest_key":key,"public_ingest_url":state.config.public_ingest_url,"ingest_url":state.config.public_ingest_url,"service_status":status["service_status"],"capabilities":capabilities(),"srt_hint":"","session":status["session"],"public_visible":false,"status_text":null,"reconnect_wait_s":wait,"dock_url_vorhanden":!dock_urls.is_null(),"dock_urls":dock_urls,"chat":chat,"uplink_sessions":status["sessions"]}),
    ))
}
async fn destinations(
    State(state): State<Arc<ServiceState>>,
    headers: HeaderMap,
    Query(query): Query<TenantQuery>,
) -> ApiResult {
    authorize(&state, &headers, query.streamer_id)?;
    let rows = state.store.query("SELECT d.platform,d.rtmp_url,d.enabled,d.width,d.height,d.fps,d.bitrate_kbps,COALESCE(f.generation,0),d.twitch_audio_mode,d.hochkant_enabled,d.hochkant_width,d.hochkant_height,d.twitch_output_mode FROM relay.destinations d LEFT JOIN relay.destination_fences f USING(streamer_id,platform) WHERE d.streamer_id=$1 ORDER BY d.platform", &[&query.streamer_id]).await.map_err(|e|failure(StatusCode::SERVICE_UNAVAILABLE,e))?;
    let hochkant_revision = state
        .store
        .query(
            "SELECT COALESCE(MAX(revision),0) FROM relay.hochkant_layouts WHERE streamer_id=$1",
            &[&query.streamer_id],
        )
        .await
        .map_err(|e| failure(StatusCode::SERVICE_UNAVAILABLE, e))?;
    let requested_revision = hochkant_revision
        .first()
        .and_then(|row| row.try_get::<_, i64>(0).ok())
        .unwrap_or(0);
    let mut outputs = Vec::with_capacity(rows.len());
    let sessions = state.registry.status(query.streamer_id as u64);
    let input = crate::media_status::input_status(sessions.iter().find(|session| session.active));
    for row in rows {
        let invalid = || {
            failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Zieldaten sind ungültig.",
            )
        };
        let endpoint = zeroize::Zeroizing::new(row.try_get::<_, String>(1).map_err(|_| invalid())?);
        let platform = row.try_get::<_, String>(0).map_err(|_| invalid())?;
        let endpoint_error = state
            .config
            .platforms
            .iter()
            .find(|policy| policy.name == platform)
            .ok_or("Für das Ziel fehlt die geprüfte Plattformkonfiguration.")
            .and_then(|policy| crate::destinations::runtime_endpoint(&platform, &endpoint, policy))
            .err();
        let blocked = endpoint_error.is_some();
        let (output_state, reason) = output_status(sessions.first(), &platform, blocked);
        let active_profile =
            crate::media_status::active_profile(sessions.first(), &platform, output_state);
        let active_profiles =
            crate::media_status::active_profiles(sessions.first(), &platform, output_state);
        let active_revision = sessions
            .first()
            .filter(|session| session.active)
            .and_then(|session| session.frozen_layouts.get(&platform))
            .and_then(|wahl| wahl["revision"].as_u64());
        let hochkant_enabled: bool = row.try_get(9).unwrap_or(false);
        // Der gespeicherte Altwert ist absichtlich keine Steuerung mehr. Wir
        // lesen die Legacy-Spalte nur noch, damit ein ungültiger DB-Datensatz
        // weiterhin sichtbar als fehlerhafte Zeile abgewiesen wird.
        // Twitch bekommt immer einen getrennten VOD-Mix; fehlt OBS-Track 2,
        // scheitert nur dieser Ausgang sichtbar statt den Live-Mix zu kopieren.
        let _legacy_requested_audio: Option<String> = row.try_get(8).map_err(|_| invalid())?;
        let effective_audio = (platform == "twitch").then_some("separate_vod");
        let active_audio =
            crate::media_status::active_audio_mode(sessions.first(), &platform, output_state);
        let active_audio_routes =
            crate::media_status::active_audio_routes(sessions.first(), &platform, output_state);
        let mode = sessions
            .first()
            .filter(|session| session.active)
            .and_then(|session| session.output_modes.get(&platform));
        let active_output_mode = mode
            .filter(|_| output_state == "sending")
            .and_then(|mode| mode.active);
        let fallback_reason = mode.and_then(|mode| mode.fallback_reason.as_deref());
        outputs.push(json!({"platform":platform,"connection_generation":row.try_get::<_,i64>(7).map_err(|_|invalid())?,"rtmp_url":if blocked {""} else {endpoint.as_str()},"enabled":row.try_get::<_,bool>(2).map_err(|_|invalid())?,"blocked":blocked,"error":endpoint_error,"requested":{"width":row.try_get::<_,Option<i32>>(3).map_err(|_|invalid())?,"height":row.try_get::<_,Option<i32>>(4).map_err(|_|invalid())?,"fps":row.try_get::<_,Option<i32>>(5).map_err(|_|invalid())?,"bitrate_kbps":row.try_get::<_,Option<i32>>(6).map_err(|_|invalid())?},"requested_output_mode":row.try_get::<_,String>(12).map_err(|_|invalid())?,"active_output_mode":active_output_mode,"fallback_reason":fallback_reason,"active_profile":active_profile,"active_profiles":active_profiles,"hochkant":{"enabled":hochkant_enabled,"width":row.try_get::<_,Option<i32>>(10).map_err(|_|invalid())?,"height":row.try_get::<_,Option<i32>>(11).map_err(|_|invalid())?,"requested_revision":requested_revision,"active_revision":active_revision},"twitch_audio_mode":serde_json::Value::Null,"effective_audio_mode":effective_audio,"active_audio_mode":active_audio,"active_audio_routes":active_audio_routes,"output_state":output_state,"reason":reason,"publication_confirmed":false,"input_codec":input["input_codec"],"input_bitrate_kbps":input["input_bitrate_kbps"]}));
    }
    Ok(Json(json!({"destinations": outputs})))
}
fn output_status(
    session: Option<&crate::registry::SessionStatus>,
    platform: &str,
    blocked: bool,
) -> (&'static str, Option<&'static str>) {
    if blocked {
        return (
            "failed",
            Some(
                "Konfiguration des Ziels ist gesperrt. Der Betreiber muss Plattformfreigabe, Serveradresse und Transport prüfen.",
            ),
        );
    }
    let Some(session) = session else {
        return ("unknown", None);
    };
    if let Some(reason) = session.blocked_outputs.get(platform) {
        return ("failed", Some(*reason));
    }
    if session.input_backpressure {
        return (
            "failed",
            Some(
                "Der Server konnte den Eingang nicht in Echtzeit verarbeiten. Die Ausgabe wurde angehalten; der Betreiber muss die verfügbare Rechenleistung prüfen.",
            ),
        );
    }
    let output = session
        .outputs
        .as_ref()
        .and_then(|s| s.get("outputs"))
        .and_then(Value::as_array)
        .and_then(|list| list.iter().find(|o| o["id"] == platform));
    match output.map(|o| &o["state"]) {
        Some(value) if value.get("failed").is_some() => (
            "failed",
            Some(crate::media_status::failure_reason(&value["failed"])),
        ),
        Some(value) if value == "interrupted" => (
            "finished",
            Some(
                "Quellverbindung unerwartet unterbrochen; die Plattform wurde nicht aktiv beendet und kann einen schnellen OBS-Reconnect übernehmen.",
            ),
        ),
        Some(value) if value == "ended" || value == "local_end_unconfirmed" => (
            "finished",
            Some("Lokaler Versand beendet; Plattformannahme ist nicht bestätigt."),
        ),
        Some(value) if value == "publishing" && session.active => {
            if output
                .and_then(|o| o["received_events"].as_u64())
                .is_some_and(|events| events > 0)
            {
                ("sending", session.output_notices.get(platform).copied())
            } else {
                (
                    "starting",
                    Some("Zielverbindung bestätigt; Medienversand wird erwartet."),
                )
            }
        }
        Some(value) if value == "starting" && session.active => {
            ("starting", session.output_notices.get(platform).copied())
        }
        _ if session.error.is_some() => ("failed", session.error),
        _ if !session.active => (
            "finished",
            Some("Session beendet; Plattformannahme ist nicht bestätigt."),
        ),
        _ => ("unknown", None),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReconnectChange {
    reconnect_wait_s: i32,
}
async fn reconnect_wait(
    State(state): State<Arc<ServiceState>>,
    headers: HeaderMap,
    Query(query): Query<TenantQuery>,
    body: Result<Json<ReconnectChange>, axum::extract::rejection::JsonRejection>,
) -> ApiResult {
    authorize(&state, &headers, query.streamer_id)?;
    let Json(body) = body.map_err(|_| {
        failure(
            StatusCode::BAD_REQUEST,
            "Wiederverbindungsfrist ist ungültig.",
        )
    })?;
    if !(0..=300).contains(&body.reconnect_wait_s) {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "Wiederverbindungsfrist muss zwischen 0 und 300 Sekunden liegen.",
        ));
    }
    let rows=state.store.query("UPDATE relay.users SET reconnect_wait_s=$2 WHERE streamer_id=$1 AND enabled=true RETURNING reconnect_wait_s",&[&query.streamer_id,&body.reconnect_wait_s]).await.map_err(|e|failure(StatusCode::SERVICE_UNAVAILABLE,e))?;
    if rows.is_empty() {
        return Err(failure(
            StatusCode::FORBIDDEN,
            "Der Zugang ist nicht freigeschaltet.",
        ));
    }
    Ok(Json(
        json!({"reconnect_wait_s":body.reconnect_wait_s,"applied":false,"message":"Frist gespeichert. Die Wiederverbindung der neuen Medienstrecke ist noch nicht freigegeben."}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Native2kHardwareUpdate {
    streamer_id: i64,
    profile: uplink_media::platform::twitch::Native2kClientProfile,
}

async fn native_2k_hardware(
    State(state): State<Arc<ServiceState>>,
    headers: HeaderMap,
    Query(query): Query<TenantQuery>,
) -> ApiResult {
    authorize(&state, &headers, query.streamer_id)?;
    let rows = state
        .store
        .query(
            "SELECT twitch_native_2k_hardware FROM relay.users WHERE streamer_id=$1 AND enabled=true",
            &[&query.streamer_id],
        )
        .await
        .map_err(|e| failure(StatusCode::SERVICE_UNAVAILABLE, e))?;
    let Some(row) = rows.first() else {
        return Err(failure(
            StatusCode::FORBIDDEN,
            "Der Zugang ist nicht freigeschaltet.",
        ));
    };
    let value: Option<serde_json::Value> = row.try_get(0).map_err(|_| {
        failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "2K-Hardwareprofil ist nicht lesbar.",
        )
    })?;
    if let Some(value) = value {
        let profile: uplink_media::platform::twitch::Native2kClientProfile =
            serde_json::from_value(value)
                .map_err(|_| failure(StatusCode::CONFLICT, "2K-Hardwareprofil ist ungültig."))?;
        profile
            .validate()
            .map_err(|_| failure(StatusCode::CONFLICT, "2K-Hardwareprofil ist ungültig."))?;
        Ok(Json(json!({"configured":true,"profile":profile})))
    } else {
        Ok(Json(json!({"configured":false,"profile":null})))
    }
}

async fn save_native_2k_hardware(
    State(state): State<Arc<ServiceState>>,
    headers: HeaderMap,
    body: Result<Json<Native2kHardwareUpdate>, axum::extract::rejection::JsonRejection>,
) -> ApiResult {
    let Json(body) = body.map_err(|_| {
        failure(
            StatusCode::BAD_REQUEST,
            "2K-Hardwareprofil enthält ungültige Daten.",
        )
    })?;
    authorize(&state, &headers, body.streamer_id)?;
    body.profile
        .validate()
        .map_err(|_| failure(StatusCode::BAD_REQUEST, "2K-Hardwareprofil ist ungültig."))?;
    let value = serde_json::to_value(&body.profile)
        .map_err(|_| failure(StatusCode::BAD_REQUEST, "2K-Hardwareprofil ist ungültig."))?;
    let rows = state
        .store
        .query(
            "UPDATE relay.users SET twitch_native_2k_hardware=$2 WHERE streamer_id=$1 AND enabled=true RETURNING streamer_id",
            &[&body.streamer_id, &value],
        )
        .await
        .map_err(|e| failure(StatusCode::SERVICE_UNAVAILABLE, e))?;
    if rows.is_empty() {
        return Err(failure(
            StatusCode::FORBIDDEN,
            "Der Zugang ist nicht freigeschaltet.",
        ));
    }
    Ok(Json(json!({
        "configured":true,
        "message":"Hardwareprofil des Quellhosts gespeichert. Die CPU-/RAM-/OS-/GPU-Daten werden für Native 2K unverändert an Twitchs GoLive-Konfiguration weitergereicht. HEVC-Passthrough benötigt 2560×1440@60 HEVC; der getrennte AV1-Modus bleibt zusätzlich an seine Server-Lastfreigabe gebunden."
    })))
}

async fn delete_native_2k_hardware(
    State(state): State<Arc<ServiceState>>,
    headers: HeaderMap,
    Query(query): Query<TenantQuery>,
) -> ApiResult {
    authorize(&state, &headers, query.streamer_id)?;
    let rows = state
        .store
        .query(
            "UPDATE relay.users SET twitch_native_2k_hardware=NULL WHERE streamer_id=$1 AND enabled=true RETURNING streamer_id",
            &[&query.streamer_id],
        )
        .await
        .map_err(|e| failure(StatusCode::SERVICE_UNAVAILABLE, e))?;
    if rows.is_empty() {
        return Err(failure(
            StatusCode::FORBIDDEN,
            "Der Zugang ist nicht freigeschaltet.",
        ));
    }
    Ok(Json(json!({"configured":false})))
}

#[derive(Deserialize)]
struct DestinationDeleteQuery {
    streamer_id: i64,
    connection_generation: i64,
}
async fn delete_destination(
    State(state): State<Arc<ServiceState>>,
    headers: HeaderMap,
    Query(query): Query<DestinationDeleteQuery>,
    Path(platform): Path<String>,
) -> ApiResult {
    authorize(&state, &headers, query.streamer_id)?;
    if query.connection_generation <= 0
        || !matches!(platform.as_str(), "twitch" | "kick" | "youtube" | "tiktok")
    {
        return Err(failure(StatusCode::BAD_REQUEST, "Ziel ist ungültig."));
    }
    let guard = state
        .registry
        .begin_change(query.streamer_id as u64)
        .map_err(|e| failure(StatusCode::CONFLICT, e))?;
    state
        .store
        .query_fenced(
            [crate::store::CheckedStatement{
                sql:"INSERT INTO relay.destination_fences AS current(streamer_id,platform,generation,deleted) VALUES($1,$2,$3,true) ON CONFLICT(streamer_id,platform) DO UPDATE SET generation=EXCLUDED.generation,deleted=true WHERE EXCLUDED.generation>=current.generation RETURNING generation",
                expected_rows:Some(1),
            },crate::store::CheckedStatement{
                sql:"DELETE FROM relay.destinations WHERE streamer_id=$1 AND platform=$2 RETURNING $3::bigint AS generation",
                expected_rows:None,
            }],
            &[&query.streamer_id, &platform,&query.connection_generation],
            Some(guard),
        )
        .await
        .map_err(|e| failure(if e.starts_with("Zielgeneration"){StatusCode::CONFLICT}else{StatusCode::SERVICE_UNAVAILABLE}, e))?;
    Ok(Json(
        json!({"platform":platform,"deleted":true,"connection_generation":query.connection_generation}),
    ))
}
fn internal_auth(
    state: &ServiceState,
    headers: &HeaderMap,
    admin: bool,
) -> Result<(), (StatusCode, Json<Value>)> {
    let secret = if admin {
        &state.secrets.admin
    } else {
        &state.secrets.api
    };
    let candidate = headers
        .get("X-Relay-Auth")
        .map_or(&[][..], |header| header.as_bytes());
    if secret.expose().is_empty() || !secret.matches(candidate) {
        return Err(failure(
            StatusCode::UNAUTHORIZED,
            "Zugriff wurde abgewiesen.",
        ));
    }
    Ok(())
}
async fn caps(State(state): State<Arc<ServiceState>>, headers: HeaderMap) -> ApiResult {
    internal_auth(&state, &headers, false)?;
    let platforms:Vec<_>=state.config.platforms.iter().map(|p|json!({"platform":p.name,"recommended_width":null,"recommended_height":null,"recommended_fps":null,"recommended_bitrate_kbps":null,"force_cbr":true,"verification":"requires_source_and_target"})).collect();
    Ok(Json(
        json!({"platforms":platforms,"ingest":null,"capabilities":capabilities(),"message":"Profile werden mit dem tatsächlichen Eingang und dem Ziel geprüft. Noch unbestätigte Empfehlungen bleiben leer."}),
    ))
}
fn admin_id(id: i64) -> Result<(), (StatusCode, Json<Value>)> {
    if id <= 0 {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "Plattformidentität ist ungültig.",
        ));
    }
    Ok(())
}
async fn admin_waitlist(State(state): State<Arc<ServiceState>>, headers: HeaderMap) -> ApiResult {
    internal_auth(&state, &headers, true)?;
    let rows=state.store.query("SELECT w.streamer_id,w.requested_at::text,w.note,COALESCE(u.enabled,false) FROM relay.waitlist w LEFT JOIN relay.users u USING(streamer_id) ORDER BY w.requested_at,w.streamer_id LIMIT 1000",&[]).await.map_err(|e|failure(StatusCode::SERVICE_UNAVAILABLE,e))?;
    let invalid = || {
        failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "Wartelistendaten sind ungültig.",
        )
    };
    let mut entries = Vec::with_capacity(rows.len());
    for row in rows {
        entries.push(json!({"streamer_id":row.try_get::<_,i64>(0).map_err(|_|invalid())?,"requested_at":row.try_get::<_,String>(1).map_err(|_|invalid())?,"note":row.try_get::<_,Option<String>>(2).map_err(|_|invalid())?,"enabled":row.try_get::<_,bool>(3).map_err(|_|invalid())?}));
    }
    Ok(Json(json!({"entries":entries})))
}
async fn reject_waitlist(
    State(state): State<Arc<ServiceState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult {
    internal_auth(&state, &headers, true)?;
    admin_id(id)?;
    let rows = state
        .store
        .query(
            "DELETE FROM relay.waitlist WHERE streamer_id=$1 RETURNING streamer_id",
            &[&id],
        )
        .await
        .map_err(|e| failure(StatusCode::SERVICE_UNAVAILABLE, e))?;
    if rows.is_empty() {
        return Err(failure(
            StatusCode::NOT_FOUND,
            "Anfrage ist nicht mehr auf der Warteliste.",
        ));
    }
    Ok(Json(json!({"streamer_id":id,"rejected":true})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Admission {
    streamer_id: i64,
}
fn invalid_credentials() -> (StatusCode, Json<Value>) {
    failure(
        StatusCode::SERVICE_UNAVAILABLE,
        "Bestehender Streamzugang ist ungültig.",
    )
}
fn credential_hash(key: &[u8]) -> Result<String, (StatusCode, Json<Value>)> {
    if key.len() != 36
        || !key.starts_with(b"rsr_")
        || !key[4..]
            .iter()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(invalid_credentials());
    }
    use sha2::Digest;
    Ok(hex::encode(sha2::Sha256::digest(key)))
}
async fn admit_user(
    State(state): State<Arc<ServiceState>>,
    headers: HeaderMap,
    body: Result<Json<Admission>, axum::extract::rejection::JsonRejection>,
) -> ApiResult {
    internal_auth(&state, &headers, true)?;
    let Json(body) =
        body.map_err(|_| failure(StatusCode::BAD_REQUEST, "Plattformidentität ist ungültig."))?;
    let id = body.streamer_id;
    admin_id(id)?;
    let existing = state
        .store
        .query(
            "SELECT ingest_key_hash,ingest_key_enc,enabled FROM relay.users WHERE streamer_id=$1",
            &[&id],
        )
        .await
        .map_err(|e| failure(StatusCode::SERVICE_UNAVAILABLE, e))?;
    let present = !existing.is_empty();
    let (previous_hash, previous_enc, previous_enabled): (Option<String>, Option<Vec<u8>>, bool) =
        match existing.first() {
            Some(row) => (
                row.try_get(0).map_err(|_| invalid_credentials())?,
                row.try_get(1).map_err(|_| invalid_credentials())?,
                row.try_get(2).map_err(|_| invalid_credentials())?,
            ),
            None => (None, None, false),
        };
    let (stored, encrypted) = if let Some(encrypted) = &previous_enc {
        let stored = state
            .secrets
            .encryption
            .open(encrypted, &format!("ingest_key:{id}"))
            .map_err(|_| invalid_credentials())?;
        let actual = credential_hash(stored.expose())?;
        if previous_hash.as_ref().is_some_and(|hash| hash != &actual) {
            return Err(invalid_credentials());
        }
        (stored, encrypted.clone())
    } else {
        // Ein vorhandener Hash ohne entschlüsselbaren Schlüssel wird nicht als
        // Freigabe-Nebenwirkung rotiert. Nur vollständig fehlende Daten anlegen.
        if previous_hash.is_some() {
            return Err(invalid_credentials());
        }
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|_| {
            failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "Zufallsquelle ist nicht verfügbar.",
            )
        })?;
        let key = crate::crypto::Secret::new(format!("rsr_{}", hex::encode(random)).into_bytes());
        let encrypted = state
            .secrets
            .encryption
            .seal(key.expose(), &format!("ingest_key:{id}"))
            .map_err(|_| invalid_credentials())?;
        (key, encrypted)
    };
    let hash = credential_hash(stored.expose())?;
    // Die Prüfung geschieht vor jeder Schreibwirkung. Nach paralleler Rotation
    // oder Sperre passt der gelesene Stand nicht mehr: nichts überschreiben.
    let rows = state.store.query("WITH admitted AS (INSERT INTO relay.users AS u(streamer_id,enabled,ingest_key_hash,ingest_key_enc) VALUES($1,true,$2,$3) ON CONFLICT(streamer_id) DO UPDATE SET enabled=true,ingest_key_hash=EXCLUDED.ingest_key_hash,ingest_key_enc=EXCLUDED.ingest_key_enc WHERE $6::bool AND u.ingest_key_hash IS NOT DISTINCT FROM $4::text AND u.ingest_key_enc IS NOT DISTINCT FROM $5::bytea AND u.enabled=$7 RETURNING streamer_id), removed AS (DELETE FROM relay.waitlist w USING admitted a WHERE w.streamer_id=a.streamer_id) SELECT streamer_id FROM admitted", &[&id,&hash,&encrypted,&previous_hash,&previous_enc,&present,&previous_enabled]).await.map_err(|e|failure(StatusCode::SERVICE_UNAVAILABLE,e))?;
    if rows.is_empty() {
        return Err(failure(
            StatusCode::CONFLICT,
            "Streamzugang wurde gleichzeitig geändert. Gespeicherten Stand neu laden.",
        ));
    }
    let key = std::str::from_utf8(stored.expose()).map_err(|_| {
        failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "Bestehender Streamzugang ist ungültig.",
        )
    })?;
    Ok(Json(
        json!({"streamer_id":id,"enabled":true,"ingest_key":key,"public_ingest_url":state.config.public_ingest_url,"srt_hint":""}),
    ))
}
