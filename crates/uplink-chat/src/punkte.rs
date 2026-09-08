//! Kanalpunkte: eine Einloesung erfuellen oder ablehnen (REQ-14).
//!
//! Twitch: `PATCH helix/channel_points/custom_rewards/redemptions` mit
//! `broadcaster_id`, `reward_id`, `id` und `{status: FULFILLED|CANCELED}`,
//! Scope `channel:manage:redemptions`. Twitch erlaubt die Statusaenderung
//! nur fuer Rewards, die mit derselben Client-Id angelegt wurden; sonst 403.
//! Das ist kein Anmeldefehler, sondern ein Hinweis fuer das Dock.

use std::sync::Arc;

use futures::future::BoxFuture;
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde_json::json;

use crate::Platform;
use crate::adapter::ChatFehler;
use crate::helix::HelixFabrik;

/// Was aus einer offenen Einloesung werden soll.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EinloesungStatus {
    Erfuellt,
    Abgelehnt,
}

impl EinloesungStatus {
    fn twitch(self) -> &'static str {
        match self {
            Self::Erfuellt => "FULFILLED",
            Self::Abgelehnt => "CANCELED",
        }
    }
}

/// Scope, den Twitch fuer die Statusaenderung verlangt.
const SCOPE: &str = "channel:manage:redemptions";

/// Hinweis, wenn Twitch die Aenderung verweigert, weil der Reward nicht mit
/// unserer App angelegt wurde.
pub const HINWEIS_FREMDER_REWARD: &str = "Twitch erlaubt das nur für Belohnungen, die über Uplink angelegt wurden. Diese Belohnung im Twitch-Dashboard erfüllen.";

pub trait PunkteDienst: Send + Sync {
    fn einloesung_setzen<'a>(
        &'a self,
        streamer_id: i64,
        platform: Platform,
        redemption_id: &'a str,
        reward_id: &'a str,
        status: EinloesungStatus,
    ) -> BoxFuture<'a, Result<(), ChatFehler>>;
}

/// Ohne Zugang zum Bot.
pub struct OhnePunkte;

impl PunkteDienst for OhnePunkte {
    fn einloesung_setzen<'a>(
        &'a self,
        _streamer_id: i64,
        platform: Platform,
        _redemption_id: &'a str,
        _reward_id: &'a str,
        _status: EinloesungStatus,
    ) -> BoxFuture<'a, Result<(), ChatFehler>> {
        Box::pin(async move { Err(ChatFehler::NichtVerbunden(platform)) })
    }
}

pub struct TwitchPunkte {
    helix: Arc<HelixFabrik>,
}

impl TwitchPunkte {
    pub fn new(helix: Arc<HelixFabrik>) -> Self {
        Self { helix }
    }
}

impl PunkteDienst for TwitchPunkte {
    fn einloesung_setzen<'a>(
        &'a self,
        streamer_id: i64,
        platform: Platform,
        redemption_id: &'a str,
        reward_id: &'a str,
        status: EinloesungStatus,
    ) -> BoxFuture<'a, Result<(), ChatFehler>> {
        Box::pin(async move {
            if platform != Platform::Twitch {
                return Err(ChatFehler::NichtUnterstuetzt(platform));
            }
            let client = self.helix.client(streamer_id);
            let zugang = client.zugang().await?;
            // Twitch antwortet auf diesen PATCH auch dann mit 403, wenn
            // `channel:manage:redemptions` fehlt. Ohne diese Pruefung liest
            // ein Streamer mit altem Grant, Twitch erlaube das nur fuer
            // Belohnungen aus Uplink, und geht ins Twitch-Dashboard statt
            // seine Verbindung zu erneuern (REQ-12).
            if !crate::twitch::scope_vorhanden(&zugang.scopes, SCOPE) {
                return Err(ChatFehler::NeuAnmeldungNoetig(Platform::Twitch));
            }
            let (code, _koerper) = client
                .anfrage(
                    Method::PATCH,
                    "/channel_points/custom_rewards/redemptions",
                    &[
                        ("broadcaster_id", zugang.platform_user_id.as_str()),
                        ("reward_id", reward_id),
                        ("id", redemption_id),
                    ],
                    Some(&json!({ "status": status.twitch() })),
                )
                .await?;
            match code {
                s if s.is_success() => Ok(()),
                StatusCode::FORBIDDEN => Err(ChatFehler::Abgelehnt(HINWEIS_FREMDER_REWARD.into())),
                StatusCode::TOO_MANY_REQUESTS => {
                    Err(ChatFehler::Abgelehnt(crate::helix::HINWEIS_BREMSE.into()))
                }
                StatusCode::NOT_FOUND => Err(ChatFehler::Abgelehnt(
                    "Einlösung ist nicht mehr offen".into(),
                )),
                StatusCode::BAD_REQUEST => {
                    // Englische Helix-Meldung nur ins Log (INV-3).
                    tracing::info!("Twitch nimmt die Einloesung nicht an");
                    Err(ChatFehler::Abgelehnt(
                        "Twitch nimmt diese Einlösung nicht an".into(),
                    ))
                }
                sonst => Err(ChatFehler::Netz(format!("redemptions: HTTP {sonst}"))),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helix::testhilfe::{bot_und_helix_mit_scopes, fabrik};
    use wiremock::matchers::{body_json, method, path, query_param};
    use wiremock::{Mock, ResponseTemplate};

    /// Twitch antwortet auch dann mit 403, wenn
    /// `channel:manage:redemptions` fehlt. Ohne die Vorpruefung liest ein
    /// Streamer mit altem Grant, Twitch erlaube das nur fuer Belohnungen aus
    /// Uplink, und geht ins Twitch-Dashboard statt seine Verbindung zu
    /// erneuern. Falsche Faehrte, dauerhaft.
    #[tokio::test]
    async fn ohne_verwaltungsscope_heisst_es_neu_anmelden_und_nicht_fremder_reward() {
        let server = bot_und_helix_mit_scopes(1, &["channel:read:redemptions"]).await;
        Mock::given(method("PATCH"))
            .and(path("/channel_points/custom_rewards/redemptions"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "error": "Forbidden", "status": 403, "message": "missing scope"
            })))
            // Ohne Scope geht der Aufruf gar nicht erst raus.
            .expect(0)
            .mount(&server)
            .await;
        let dienst = TwitchPunkte::new(Arc::new(fabrik(&server)));
        let fehler = dienst
            .einloesung_setzen(
                7,
                Platform::Twitch,
                "r-1",
                "rw-1",
                EinloesungStatus::Erfuellt,
            )
            .await
            .expect_err("ohne Scope kein Erfolg");
        assert_eq!(fehler, ChatFehler::NeuAnmeldungNoetig(Platform::Twitch));
        server.verify().await;
    }

    #[tokio::test]
    async fn einloesung_setzen_403_hat_eigenen_hinweis() {
        let server = bot_und_helix_mit_scopes(1, &["channel:manage:redemptions"]).await;
        Mock::given(method("PATCH"))
            .and(path("/channel_points/custom_rewards/redemptions"))
            .and(query_param("broadcaster_id", "12345"))
            .and(query_param("reward_id", "rw-1"))
            .and(query_param("id", "r-1"))
            .and(body_json(json!({ "status": "FULFILLED" })))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "error": "Forbidden", "status": 403,
                "message": "The custom reward was created by a different client_id."
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/channel_points/custom_rewards/redemptions"))
            .and(query_param("id", "r-2"))
            .and(body_json(json!({ "status": "CANCELED" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [] })))
            .expect(1)
            .mount(&server)
            .await;
        let dienst = TwitchPunkte::new(Arc::new(fabrik(&server)));
        let fehler = dienst
            .einloesung_setzen(
                7,
                Platform::Twitch,
                "r-1",
                "rw-1",
                EinloesungStatus::Erfuellt,
            )
            .await
            .expect_err("403 ist ein Fehler");
        assert_eq!(fehler, ChatFehler::Abgelehnt(HINWEIS_FREMDER_REWARD.into()));
        assert!(
            crate::supervisor::hinweis_fuer(&fehler).contains("Twitch-Dashboard"),
            "Hinweis in Nutzersprache, kein Neu-Anmelden"
        );
        assert_eq!(
            dienst
                .einloesung_setzen(
                    7,
                    Platform::Twitch,
                    "r-2",
                    "rw-1",
                    EinloesungStatus::Abgelehnt
                )
                .await,
            Ok(())
        );
        assert_eq!(
            dienst
                .einloesung_setzen(
                    7,
                    Platform::Kick,
                    "r-2",
                    "rw-1",
                    EinloesungStatus::Abgelehnt
                )
                .await
                .err(),
            Some(ChatFehler::NichtUnterstuetzt(Platform::Kick))
        );
        server.verify().await;
    }
}
