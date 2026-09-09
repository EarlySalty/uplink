//! Bestehende Dockidentitäten und autoritativer Botbroker, ohne OAuth-Speicher.
use crate::{crypto::Secret, store::Store};
use serde::de::DeserializeOwned;
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};
use uplink_chat::{BrokerError, DockIdentity, DockUser, Grant, Platform, PlatformBroker};

type Reply<'a, T> = Pin<Box<dyn Future<Output = Result<T, BrokerError>> + Send + 'a>>;

pub struct StoredDockIdentity(pub Arc<Store>);
impl DockIdentity for StoredDockIdentity {
    fn resolve(&self, hash: [u8; 32]) -> Reply<'_, Option<DockUser>> {
        Box::pin(async move {
            let rows = self
                .0
                .query(
                    "SELECT streamer_id,enabled FROM relay.users WHERE dock_token_hash=$1",
                    &[&hex::encode(hash)],
                )
                .await
                .map_err(|_| BrokerError::Unavailable)?;
            if rows.len() > 1 {
                return Err(BrokerError::Unauthorized);
            }
            let Some(row) = rows.first() else {
                return Ok(None);
            };
            let id: i64 = row.try_get(0).map_err(|_| BrokerError::Unavailable)?;
            let streamer_id = u64::try_from(id)
                .ok()
                .filter(|id| *id > 0)
                .ok_or(BrokerError::Unauthorized)?;
            Ok(Some(DockUser {
                streamer_id,
                enabled: row.try_get(1).map_err(|_| BrokerError::Unavailable)?,
            }))
        })
    }
}

pub struct BotBroker {
    client: reqwest::Client,
    base: reqwest::Url,
    token: Secret,
    requests: tokio::sync::Semaphore,
}
impl BotBroker {
    pub fn new(base: &str, token: Secret) -> Result<Self, &'static str> {
        let base = reqwest::Url::parse(base).map_err(|_| "Botadresse ist ungültig.")?;
        if !matches!(base.scheme(), "http" | "https")
            || !matches!(base.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
            || base.path() != "/"
            || base.query().is_some()
            || base.fragment().is_some()
            || !base.username().is_empty()
            || base.password().is_some()
            || token.expose().is_empty()
            || token.expose().len() > 8192
        {
            return Err(
                "Bot benötigt eine explizite lokale Adresse und einen gültigen Dienstzugang.",
            );
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(4))
            .build()
            .map_err(|_| "Botverbindung konnte nicht vorbereitet werden.")?;
        Ok(Self {
            client,
            base,
            token,
            requests: tokio::sync::Semaphore::new(32),
        })
    }
    async fn get(
        &self,
        id: u64,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<(u16, zeroize::Zeroizing<Vec<u8>>), BrokerError> {
        if id == 0 || id > i64::MAX as u64 {
            return Err(BrokerError::Unauthorized);
        }
        let _slot = self
            .requests
            .try_acquire()
            .map_err(|_| BrokerError::Unavailable)?;
        let url = self.base.join(path).map_err(|_| BrokerError::Unavailable)?;
        let mut response = self
            .client
            .get(url)
            .header("X-Internal-Token", self.token.expose())
            .query(&[("streamer", id.to_string())])
            .query(query)
            .send()
            .await
            .map_err(|_| BrokerError::Unavailable)?;
        let status = response.status().as_u16();
        let mut body = zeroize::Zeroizing::new(Vec::new());
        const MAX: usize = 256 * 1024;
        if response.content_length().is_some_and(|n| n > MAX as u64) {
            return Err(BrokerError::Unavailable);
        }
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| BrokerError::Unavailable)?
        {
            if body.len().saturating_add(chunk.len()) > MAX {
                return Err(BrokerError::Unavailable);
            }
            body.extend_from_slice(&chunk);
        }
        Ok((status, body))
    }
    pub async fn publish_grant(&self, id: u64) -> Result<i64, BrokerError> {
        let (status, body) = self
            .get(
                id,
                "/twitch/api/v2/internal/platform-token",
                &[("platform", "twitch".into()), ("purpose", "publish".into())],
            )
            .await?;
        if status != 200 {
            return Err(match status {
                401 | 403 | 409 => BrokerError::Unauthorized,
                404 => BrokerError::AccessUnconfirmed,
                _ => BrokerError::Unavailable,
            });
        }
        validate_publish_grant(id, &body)
    }

    fn decode<T: DeserializeOwned>(body: &[u8]) -> Result<T, BrokerError> {
        serde_json::from_slice(body).map_err(|_| BrokerError::Unavailable)
    }
}
impl PlatformBroker for BotBroker {
    fn grant(&self, id: u64, platform: Platform, _refresh: bool) -> Reply<'_, Grant> {
        // Regulärer Brokeraufruf führt seinen vorhandenen Refresh aus. Es gibt
        // keinen bestätigten force_refresh-Parameter und keinen zweiten Cache.
        Box::pin(async move {
            let (status, body) = self
                .get(
                    id,
                    "/twitch/api/v2/internal/platform-token",
                    &[("platform", platform.as_str().into())],
                )
                .await?;
            if status == 200 {
                let grant: Grant = Self::decode(&body)?;
                if platform == Platform::Twitch && grant.platform_user_id != id.to_string() {
                    return Err(BrokerError::Unauthorized);
                }
                if grant.access_token.is_empty()
                    || grant.access_token.len() > 8192
                    || grant.platform_user_id.is_empty()
                    || grant.platform_user_id.len() > 128
                    || grant.platform_user_id.chars().any(char::is_control)
                    || grant.platform_login.len() > 128
                    || grant.scopes.len() > 128
                    || grant
                        .scopes
                        .iter()
                        .any(|s| s.len() > 128 || s.chars().any(char::is_control))
                {
                    return Err(BrokerError::Unavailable);
                }
                return Ok(grant);
            }
            #[derive(serde::Deserialize)]
            struct ErrorCode {
                error_code: Option<String>,
            }
            let code = Self::decode::<ErrorCode>(&body)
                .ok()
                .and_then(|r| r.error_code);
            Err(match (status, code.as_deref()) {
                (401 | 403, _) | (_, Some("identity_mismatch")) => BrokerError::Unauthorized,
                (_, Some("connection_missing")) => BrokerError::Disconnected,
                (409, _) | (_, Some("reauthorization_required")) => BrokerError::NeedsReauth,
                (404, _) | (_, Some("missing_scope")) => BrokerError::AccessUnconfirmed,
                _ => BrokerError::Unavailable,
            })
        })
    }
    fn metrics(&self, id: u64) -> Reply<'_, Option<uplink_chat::kennzahlen::BotKennzahlen>> {
        Box::pin(async move {
            let (status, body) = self
                .get(id, "/twitch/api/v2/internal/stream-kennzahlen", &[])
                .await?;
            match status {
                200 => Self::decode(&body).map(Some),
                404 => Ok(None),
                _ => Err(BrokerError::Unavailable),
            }
        })
    }
    fn chatter_history<'a>(
        &'a self,
        id: u64,
        logins: &'a [String],
    ) -> Reply<'a, Vec<uplink_chat::hervorhebung::VerlaufEintrag>> {
        Box::pin(async move {
            if logins.len() > 100
                || logins.iter().any(|s| {
                    s.is_empty()
                        || s.len() > 128
                        || s.chars().any(char::is_control)
                        || s.contains(',')
                })
            {
                return Err(BrokerError::Unavailable);
            }
            let (status, body) = self
                .get(
                    id,
                    "/twitch/api/v2/internal/chatter-verlauf",
                    &[("logins", logins.join(","))],
                )
                .await?;
            #[derive(serde::Deserialize)]
            struct History {
                eintraege: Vec<uplink_chat::hervorhebung::VerlaufEintrag>,
            }
            match status {
                200 => Self::decode::<History>(&body).map(|r| r.eintraege),
                404 => Ok(vec![]),
                _ => Err(BrokerError::Unavailable),
            }
        })
    }
}

fn validate_publish_grant(id: u64, body: &[u8]) -> Result<i64, BrokerError> {
    #[derive(serde::Deserialize)]
    struct PublishGrant<'a> {
        purpose: &'a str,
        token_owner: &'a str,
        platform_user_id: &'a str,
        scopes: Vec<&'a str>,
        access_token: &'a str,
        connection_generation: i64,
    }
    let grant: PublishGrant<'_> =
        serde_json::from_slice(body).map_err(|_| BrokerError::AccessUnconfirmed)?;
    if id == 0
        || grant.purpose != "publish"
        || grant.token_owner != id.to_string()
        || grant.platform_user_id != id.to_string()
        || grant.access_token.is_empty()
        || grant.access_token.len() > 8192
        || grant.scopes.len() > 128
        || !grant.scopes.contains(&"channel:read:stream_key")
        || grant.connection_generation < 1
    {
        return Err(BrokerError::Unauthorized);
    }
    Ok(grant.connection_generation)
}

#[cfg(test)]
mod publish_tests {
    use super::*;
    #[test]
    fn publish_requires_explicit_purpose_owner_and_scope() {
        let valid = serde_json::json!({"purpose":"publish","token_owner":"11","platform_user_id":"11","scopes":["channel:read:stream_key"],"access_token":"synthetic-test","connection_generation":3});
        assert_eq!(
            validate_publish_grant(11, &serde_json::to_vec(&valid).unwrap()).unwrap(),
            3
        );
        for field in [
            "purpose",
            "token_owner",
            "platform_user_id",
            "scopes",
            "connection_generation",
        ] {
            let mut invalid = valid.clone();
            invalid.as_object_mut().unwrap().remove(field);
            assert!(validate_publish_grant(11, &serde_json::to_vec(&invalid).unwrap()).is_err());
        }
        assert!(validate_publish_grant(12, &serde_json::to_vec(&valid).unwrap()).is_err());
        let mut wrong = valid;
        wrong["token_owner"] = serde_json::json!("12");
        assert!(validate_publish_grant(11, &serde_json::to_vec(&wrong).unwrap()).is_err());
    }

    #[test]
    fn publish_requires_positive_connection_generation() {
        let mut grant = serde_json::json!({"purpose":"publish","token_owner":"11","platform_user_id":"11","scopes":["channel:read:stream_key"],"access_token":"synthetic-test","connection_generation":1});
        assert_eq!(
            validate_publish_grant(11, &serde_json::to_vec(&grant).unwrap()).unwrap(),
            1
        );
        for generation in [0, -4] {
            grant["connection_generation"] = serde_json::json!(generation);
            assert!(validate_publish_grant(11, &serde_json::to_vec(&grant).unwrap()).is_err());
        }
        let mut no_scope = grant;
        no_scope["scopes"] = serde_json::json!(["chat:read"]);
        assert!(validate_publish_grant(11, &serde_json::to_vec(&no_scope).unwrap()).is_err());
    }
}
