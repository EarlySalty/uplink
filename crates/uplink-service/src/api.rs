use crate::{config::Config, registry::Registry, secrets::ServiceSecrets, store::Store};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

pub struct ServiceState {
    pub config: Config,
    pub store: Arc<Store>,
    pub secrets: Arc<ServiceSecrets>,
    pub registry: Registry,
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
    if !state.secrets.api.matches(candidate) || tenant <= 0 {
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
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/me", get(me))
        .route(
            "/v1/me/destinations",
            get(destinations).put(save_destinations),
        )
        .route("/v1/me/status", get(status))
        .route("/v1/me/waitlist", post(waitlist))
        .route("/v1/me/key/rotate", post(rotate_key))
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
                    let mut response = match tokio::time::timeout(
                        std::time::Duration::from_secs(timeout_seconds),
                        next.run(request),
                    )
                    .await
                    {
                        Ok(response) => response,
                        Err(_) => failure(
                            StatusCode::GATEWAY_TIMEOUT,
                            "Anfrage hat die Frist überschritten.",
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
        .with_state(state)
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
        rows.push(json!({"platform":output.platform,"rtmp_url":output.rtmp_url,"stream_key_enc":ciphertext.map(|bytes|format!("\\x{}",hex::encode(bytes))),"enabled":output.enabled,"width":output.width,"height":output.height,"fps":output.fps,"bitrate_kbps":output.bitrate_kbps}));
    }
    let data = json!(rows);
    let stored=state.store.query("WITH incoming AS (SELECT * FROM jsonb_to_recordset($2::jsonb) AS x(platform text,rtmp_url text,stream_key_enc bytea,enabled boolean,width integer,height integer,fps integer,bitrate_kbps integer)) INSERT INTO relay.destinations(streamer_id,platform,rtmp_url,stream_key_enc,enabled,width,height,fps,bitrate_kbps) SELECT $1,i.platform,COALESCE(i.rtmp_url,p.rtmp_url),COALESCE(i.stream_key_enc,p.stream_key_enc),COALESCE(i.enabled,p.enabled,true),COALESCE(i.width,p.width),COALESCE(i.height,p.height),COALESCE(i.fps,p.fps),COALESCE(i.bitrate_kbps,p.bitrate_kbps) FROM incoming i LEFT JOIN relay.destinations p ON p.streamer_id=$1 AND p.platform=i.platform WHERE EXISTS(SELECT 1 FROM relay.users WHERE streamer_id=$1 AND enabled=true) ON CONFLICT(streamer_id,platform) DO UPDATE SET rtmp_url=EXCLUDED.rtmp_url,stream_key_enc=EXCLUDED.stream_key_enc,enabled=EXCLUDED.enabled,width=EXCLUDED.width,height=EXCLUDED.height,fps=EXCLUDED.fps,bitrate_kbps=EXCLUDED.bitrate_kbps RETURNING platform",&[&request.streamer_id,&data]).await.map_err(|e|failure(StatusCode::SERVICE_UNAVAILABLE,e))?;
    if stored.len() != request.destinations.len() {
        return Err(failure(
            StatusCode::FORBIDDEN,
            "Der Zugang ist nicht freigeschaltet.",
        ));
    }
    Ok(Json(
        json!({"ok":true,"live_quality":{"status":"next_stream","message":"Das Wunschprofil ist gespeichert und wird beim nächsten Stream anhand des Eingangs geprüft."}}),
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
async fn health(State(state): State<Arc<ServiceState>>) -> Json<Value> {
    Json(
        json!({"ok":state.store.ready(),"service":"uplink", "active_sessions":state.registry.active_count()}),
    )
}
async fn status(
    State(state): State<Arc<ServiceState>>,
    headers: HeaderMap,
    Query(query): Query<TenantQuery>,
) -> ApiResult {
    authorize(&state, &headers, query.streamer_id)?;
    Ok(Json(
        json!({"sessions": state.registry.status(query.streamer_id as u64)}),
    ))
}
async fn me(
    State(state): State<Arc<ServiceState>>,
    headers: HeaderMap,
    Query(query): Query<TenantQuery>,
) -> ApiResult {
    authorize(&state, &headers, query.streamer_id)?;
    let rows = state.store.query("SELECT enabled,ingest_key_enc,dock_token_enc,reconnect_wait_s FROM relay.users WHERE streamer_id=$1", &[&query.streamer_id]).await.map_err(|e| failure(StatusCode::SERVICE_UNAVAILABLE,e))?;
    let Some(row) = rows.first() else {
        return Ok(Json(
            json!({"enabled":false,"waitlisted":false,"ingest_key":"","ingest_url":state.config.public_ingest_url,"srt_hint":"","session":null,"public_visible":false,"status_text":"Zugang ist noch nicht freigeschaltet.","reconnect_wait_s":0,"reconnect_wait_max_s":0,"dock_url_vorhanden":false,"dock_urls":null,"chat":[]}),
        ));
    };
    let enabled: bool = row.try_get(0).map_err(|_| {
        failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Nutzerdaten sind ungültig.",
        )
    })?;
    let encrypted: Option<Vec<u8>> = row.try_get(1).map_err(|_| {
        failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Streamzugang ist nicht verfügbar.",
        )
    })?;
    let key = encrypted
        .as_ref()
        .map(|encrypted| {
            state
                .secrets
                .encryption
                .open(encrypted, &format!("ingest_key:{}", query.streamer_id))
        })
        .transpose()
        .map_err(|e| failure(StatusCode::SERVICE_UNAVAILABLE, e))?;
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
    let dock_urls = if let Some(dock) = dock {
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
        let base = state.config.dock_base_url.trim_end_matches('/');
        json!({"chat":format!("{base}/dock/chat?t={token}"),"activity":format!("{base}/dock/activity?t={token}"),"stream_info":format!("{base}/dock/stream-info?t={token}"),"points":format!("{base}/dock/points?t={token}")})
    } else {
        Value::Null
    };
    let wait: i32 = row.try_get(3).map_err(|_| {
        failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Nutzerdaten sind ungültig.",
        )
    })?;
    Ok(Json(
        json!({"enabled":enabled,"waitlisted":false,"ingest_key":key,"ingest_url":state.config.public_ingest_url,"srt_hint":"","session":null,"public_visible":false,"status_text":null,"reconnect_wait_s":wait,"reconnect_wait_max_s":300,"dock_url_vorhanden":!dock_urls.is_null(),"dock_urls":dock_urls,"chat":[],"uplink_sessions":state.registry.status(query.streamer_id as u64)}),
    ))
}
async fn destinations(
    State(state): State<Arc<ServiceState>>,
    headers: HeaderMap,
    Query(query): Query<TenantQuery>,
) -> ApiResult {
    authorize(&state, &headers, query.streamer_id)?;
    let rows = state.store.query("SELECT platform,rtmp_url,enabled,width,height,fps,bitrate_kbps FROM relay.destinations WHERE streamer_id=$1 ORDER BY platform", &[&query.streamer_id]).await.map_err(|e|failure(StatusCode::SERVICE_UNAVAILABLE,e))?;
    let mut outputs = Vec::with_capacity(rows.len());
    for row in rows {
        let invalid = || {
            failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Zieldaten sind ungültig.",
            )
        };
        outputs.push(json!({"platform":row.try_get::<_,String>(0).map_err(|_|invalid())?,"rtmp_url":row.try_get::<_,String>(1).map_err(|_|invalid())?,"enabled":row.try_get::<_,bool>(2).map_err(|_|invalid())?,"requested":{"width":row.try_get::<_,Option<i32>>(3).map_err(|_|invalid())?,"height":row.try_get::<_,Option<i32>>(4).map_err(|_|invalid())?,"fps":row.try_get::<_,Option<i32>>(5).map_err(|_|invalid())?,"bitrate_kbps":row.try_get::<_,Option<i32>>(6).map_err(|_|invalid())?}}));
    }
    Ok(Json(json!({"destinations": outputs})))
}
