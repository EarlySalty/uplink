//! Stream-Infos lesen und setzen: Titel, Kategorie, Tags, je Plattform.
//!
//! Frage und Antwort, keine Session: der Streamer aendert den Titel auch
//! vor dem Stream. Deshalb haengt das nicht am Supervisor, sondern holt sich
//! den Zugang direkt ueber die [`crate::token::TokenQuelle`].
//!
//! [`StreamInfoDienst`] fragt alle Plattformen; wer nicht verbunden ist oder
//! noch keinen Adapter hat, wird still uebersprungen, ein Fehler einer
//! Plattform haelt die anderen nicht auf (REQ-11).

pub mod twitch;

use std::sync::Arc;

use futures::future::BoxFuture;
use serde::Serialize;

use crate::Platform;
use crate::adapter::ChatFehler;
use crate::ereignis::{StreamInfo, StreamInfoPatch};

/// Eine Kategorie aus der Suche der Plattform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Kategorie {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bild: Option<String>,
}

/// Stream-Info-Adapter einer Plattform.
pub trait StreamInfoAdapter: Send + Sync {
    /// Teil der Schnittstelle je Plattform; der Dienst kennt die Plattform
    /// schon aus der Fabrik, im Dienst ruft es bisher niemand.
    #[allow(dead_code)]
    fn platform(&self) -> Platform;
    fn lesen(&self) -> BoxFuture<'_, Result<StreamInfo, ChatFehler>>;
    fn setzen<'a>(&'a self, patch: &'a StreamInfoPatch) -> BoxFuture<'a, Result<(), ChatFehler>>;
    fn kategorien_suchen<'a>(
        &'a self,
        suche: &'a str,
    ) -> BoxFuture<'a, Result<Vec<Kategorie>, ChatFehler>>;
}

/// Baut Adapter je Streamer und Plattform. Im Dienst Twitch, im Test Fake.
pub trait StreamInfoFabrik: Send + Sync {
    fn bauen(
        &self,
        streamer_id: i64,
        platform: Platform,
    ) -> BoxFuture<'_, Result<Arc<dyn StreamInfoAdapter>, ChatFehler>>;
}

/// Fabrik ohne Zugang zum Bot.
pub struct OhneStreamInfo;

impl StreamInfoFabrik for OhneStreamInfo {
    fn bauen(
        &self,
        _streamer_id: i64,
        platform: Platform,
    ) -> BoxFuture<'_, Result<Arc<dyn StreamInfoAdapter>, ChatFehler>> {
        Box::pin(async move { Err(ChatFehler::NichtVerbunden(platform)) })
    }
}

/// Ergebnis einer Plattform, wie es ans Dock geht.
#[derive(Debug, Clone, Serialize)]
pub struct PlattformErgebnis<T: Serialize> {
    pub platform: Platform,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hinweis: Option<String>,
}

pub struct StreamInfoDienst {
    fabrik: Arc<dyn StreamInfoFabrik>,
}

impl StreamInfoDienst {
    pub fn new(fabrik: Arc<dyn StreamInfoFabrik>) -> Arc<Self> {
        Arc::new(Self { fabrik })
    }

    /// Adapter aller Plattformen, die verbunden sind und einen Adapter haben,
    /// in Reihenfolge der Wichtigkeit. Fehler beim Bauen (ausser
    /// "nicht verbunden" und "kein Adapter") kommen als Ergebnis zurueck.
    async fn adapter(
        &self,
        streamer_id: i64,
    ) -> Vec<(Platform, Result<Arc<dyn StreamInfoAdapter>, ChatFehler>)> {
        let mut liste = Vec::new();
        for platform in Platform::ALL {
            match self.fabrik.bauen(streamer_id, platform).await {
                Ok(adapter) => liste.push((platform, Ok(adapter))),
                Err(ChatFehler::NichtVerbunden(_)) | Err(ChatFehler::NichtUnterstuetzt(_)) => {}
                Err(fehler) => liste.push((platform, Err(fehler))),
            }
        }
        liste.sort_by_key(|(p, _)| p.rang());
        liste
    }

    /// Liest die Infos aller verbundenen Plattformen.
    pub async fn lesen_alle(&self, streamer_id: i64) -> Vec<PlattformErgebnis<StreamInfo>> {
        let mut ergebnisse = Vec::new();
        for (platform, adapter) in self.adapter(streamer_id).await {
            let ergebnis = match adapter {
                Ok(adapter) => adapter.lesen().await,
                Err(fehler) => Err(fehler),
            };
            ergebnisse.push(ergebnis_aus(platform, ergebnis.map(Some)));
        }
        ergebnisse
    }

    /// Schreibt den Patch an alle verbundenen Plattformen, Ergebnis je
    /// Plattform. Ein leerer Patch geht nirgends hin.
    pub async fn setzen_alle(
        &self,
        streamer_id: i64,
        patch: &StreamInfoPatch,
    ) -> Vec<PlattformErgebnis<StreamInfo>> {
        if patch.is_empty() {
            return Vec::new();
        }
        let mut ergebnisse = Vec::new();
        for (platform, adapter) in self.adapter(streamer_id).await {
            let eintrag = match adapter {
                Ok(adapter) => match adapter.setzen(patch).await {
                    // Nach dem Setzen den Stand lesen, damit das Dock die
                    // Wahrheit der Plattform zeigt (Twitch normalisiert Tags).
                    Ok(()) => match adapter.lesen().await {
                        Ok(info) => ergebnis_aus(platform, Ok(Some(info))),
                        // Gespeichert ist gespeichert. Frueher fiel der
                        // Lesefehler unter den Tisch und das Dock zeigte
                        // einen Erfolg ohne Inhalt, ohne zu sagen warum.
                        Err(fehler) => {
                            tracing::info!(
                                plattform = platform.as_str(),
                                %fehler,
                                "Stream-Infos gespeichert, der Stand danach war nicht lesbar"
                            );
                            PlattformErgebnis {
                                platform,
                                ok: true,
                                info: None,
                                hinweis: Some(
                                    "Gespeichert. Der aktuelle Stand ließ sich gerade nicht lesen."
                                        .into(),
                                ),
                            }
                        }
                    },
                    Err(fehler) => ergebnis_aus(platform, Err(fehler)),
                },
                Err(fehler) => ergebnis_aus(platform, Err(fehler)),
            };
            ergebnisse.push(eintrag);
        }
        ergebnisse
    }

    /// Kategorien je Plattform zu einem Suchwort.
    pub async fn kategorien(
        &self,
        streamer_id: i64,
        suche: &str,
    ) -> Vec<PlattformErgebnis<Vec<Kategorie>>> {
        let mut ergebnisse = Vec::new();
        for (platform, adapter) in self.adapter(streamer_id).await {
            let ergebnis = match adapter {
                Ok(adapter) => adapter.kategorien_suchen(suche).await,
                Err(fehler) => Err(fehler),
            };
            ergebnisse.push(ergebnis_aus(platform, ergebnis.map(Some)));
        }
        ergebnisse
    }
}

fn ergebnis_aus<T: Serialize>(
    platform: Platform,
    ergebnis: Result<Option<T>, ChatFehler>,
) -> PlattformErgebnis<T> {
    match ergebnis {
        Ok(info) => PlattformErgebnis {
            platform,
            ok: true,
            info,
            hinweis: None,
        },
        Err(fehler) => PlattformErgebnis {
            platform,
            ok: false,
            info: None,
            hinweis: Some(crate::supervisor::hinweis_ohne_chat(&fehler)),
        },
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! Fake-Adapter fuer Routen- und Diensttests.

    use std::sync::{Arc, Mutex};

    use futures::future::BoxFuture;

    use super::*;

    pub struct FakeStreamInfo {
        pub platform: Platform,
        pub stand: Mutex<StreamInfo>,
        pub patches: Mutex<Vec<StreamInfoPatch>>,
        pub setzen_fehler: Option<ChatFehler>,
        /// Das Nachlesen nach dem Setzen scheitert.
        pub lesen_fehler: Option<ChatFehler>,
    }

    impl StreamInfoAdapter for FakeStreamInfo {
        fn platform(&self) -> Platform {
            self.platform
        }
        fn lesen(&self) -> BoxFuture<'_, Result<StreamInfo, ChatFehler>> {
            Box::pin(async move {
                match &self.lesen_fehler {
                    Some(fehler) => Err(fehler.clone()),
                    None => Ok(self.stand.lock().unwrap().clone()),
                }
            })
        }
        fn setzen<'a>(
            &'a self,
            patch: &'a StreamInfoPatch,
        ) -> BoxFuture<'a, Result<(), ChatFehler>> {
            Box::pin(async move {
                self.patches.lock().unwrap().push(patch.clone());
                if let Some(fehler) = &self.setzen_fehler {
                    return Err(fehler.clone());
                }
                let mut stand = self.stand.lock().unwrap();
                if let Some(t) = &patch.title {
                    stand.title = t.clone();
                }
                if let Some(k) = &patch.category_id {
                    stand.category_id = Some(k.clone());
                }
                if let Some(tags) = &patch.tags {
                    stand.tags = Some(tags.clone());
                }
                Ok(())
            })
        }
        fn kategorien_suchen<'a>(
            &'a self,
            suche: &'a str,
        ) -> BoxFuture<'a, Result<Vec<Kategorie>, ChatFehler>> {
            Box::pin(async move {
                Ok(vec![Kategorie {
                    id: format!("{}-1", self.platform),
                    name: format!("{suche} {}", self.platform.anzeige()),
                    bild: None,
                }])
            })
        }
    }

    /// Adapter fuer `mit_zugang`, `NichtVerbunden` fuer den Rest; optional
    /// eine Plattform, deren Setzen fehlschlaegt.
    pub struct FakeStreamInfoFabrik {
        pub mit_zugang: Vec<Platform>,
        pub setzen_fehler: Option<(Platform, ChatFehler)>,
        pub lesen_fehler: Option<(Platform, ChatFehler)>,
        pub gebaut: Mutex<Vec<(i64, Arc<FakeStreamInfo>)>>,
    }

    impl FakeStreamInfoFabrik {
        pub fn mit(plattformen: &[Platform]) -> Arc<Self> {
            Arc::new(Self {
                mit_zugang: plattformen.to_vec(),
                setzen_fehler: None,
                lesen_fehler: None,
                gebaut: Mutex::new(Vec::new()),
            })
        }

        pub fn adapter(&self, streamer_id: i64) -> Vec<Arc<FakeStreamInfo>> {
            self.gebaut
                .lock()
                .unwrap()
                .iter()
                .filter(|(id, _)| *id == streamer_id)
                .map(|(_, a)| a.clone())
                .collect()
        }
    }

    impl StreamInfoFabrik for FakeStreamInfoFabrik {
        fn bauen(
            &self,
            streamer_id: i64,
            platform: Platform,
        ) -> BoxFuture<'_, Result<Arc<dyn StreamInfoAdapter>, ChatFehler>> {
            Box::pin(async move {
                if !self.mit_zugang.contains(&platform) {
                    return Err(ChatFehler::NichtVerbunden(platform));
                }
                let adapter = Arc::new(FakeStreamInfo {
                    platform,
                    stand: Mutex::new(StreamInfo {
                        platform,
                        channel_id: "12345".into(),
                        title: format!("Titel {}", platform.anzeige()),
                        category_id: Some("509658".into()),
                        category_name: Some("Just Chatting".into()),
                        category_bild: None,
                        tags: Some(vec!["deutsch".into()]),
                        is_live: None,
                        started_at: None,
                        viewers: None,
                    }),
                    patches: Mutex::new(Vec::new()),
                    setzen_fehler: self
                        .setzen_fehler
                        .as_ref()
                        .filter(|(p, _)| *p == platform)
                        .map(|(_, f)| f.clone()),
                    lesen_fehler: self
                        .lesen_fehler
                        .as_ref()
                        .filter(|(p, _)| *p == platform)
                        .map(|(_, f)| f.clone()),
                });
                self.gebaut
                    .lock()
                    .unwrap()
                    .push((streamer_id, adapter.clone()));
                let dynamisch: Arc<dyn StreamInfoAdapter> = adapter;
                Ok(dynamisch)
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeStreamInfoFabrik;
    use super::*;

    #[tokio::test]
    async fn stream_info_dienst_ueberspringt_nicht_verbundene() {
        let fabrik = FakeStreamInfoFabrik::mit(&[Platform::Twitch, Platform::Kick]);
        let dienst = StreamInfoDienst::new(fabrik.clone());
        let gelesen = dienst.lesen_alle(7).await;
        assert_eq!(gelesen.len(), 2, "YouTube und TikTok fehlen still");
        assert_eq!(
            gelesen[0].platform,
            Platform::Twitch,
            "Reihenfolge nach Rang"
        );
        assert_eq!(gelesen[1].platform, Platform::Kick);
        assert!(gelesen.iter().all(|e| e.ok && e.info.is_some()));

        let patch = StreamInfoPatch {
            title: Some("Neu".into()),
            ..Default::default()
        };
        let gesetzt = dienst.setzen_alle(7, &patch).await;
        assert_eq!(gesetzt.len(), 2);
        assert!(gesetzt.iter().all(|e| e.ok));
        assert_eq!(gesetzt[0].info.as_ref().unwrap().title, "Neu");
        // Je Aufruf frische Adapter: die aus `setzen_alle` tragen den Patch.
        let patches: usize = fabrik
            .adapter(7)
            .iter()
            .map(|a| a.patches.lock().unwrap().len())
            .sum();
        assert_eq!(patches, 2, "ein Patch je verbundener Plattform");
        assert!(
            dienst
                .setzen_alle(7, &StreamInfoPatch::default())
                .await
                .is_empty(),
            "leerer Patch geht nirgends hin"
        );
        let kategorien = dienst.kategorien(7, "dead").await;
        assert_eq!(kategorien[0].info.as_ref().unwrap()[0].name, "dead Twitch");

        let ohne = StreamInfoDienst::new(Arc::new(OhneStreamInfo));
        assert!(ohne.lesen_alle(7).await.is_empty());
    }

    /// Twitch nimmt den PATCH an, das anschliessende Lesen laeuft in einen
    /// Fehler. Frueher fiel der unter den Tisch: das Dock sah `ok: true` ohne
    /// `info` und ohne Grund.
    #[tokio::test]
    async fn lesefehler_nach_dem_speichern_kommt_als_hinweis_durch() {
        let fabrik = Arc::new(FakeStreamInfoFabrik {
            mit_zugang: vec![Platform::Twitch],
            setzen_fehler: None,
            lesen_fehler: Some((
                Platform::Twitch,
                ChatFehler::Netz("channels: HTTP 500".into()),
            )),
            gebaut: std::sync::Mutex::new(Vec::new()),
        });
        let dienst = StreamInfoDienst::new(fabrik);
        let patch = StreamInfoPatch {
            title: Some("Neu".into()),
            ..Default::default()
        };
        let ergebnisse = dienst.setzen_alle(7, &patch).await;
        assert_eq!(ergebnisse.len(), 1);
        assert!(ergebnisse[0].ok, "gespeichert ist gespeichert");
        assert!(ergebnisse[0].info.is_none());
        let hinweis = ergebnisse[0].hinweis.as_deref().unwrap_or_default();
        assert!(
            hinweis.contains("Gespeichert"),
            "der Streamer soll erfahren, dass nur das Nachlesen fehlte: {hinweis}"
        );
    }

    #[tokio::test]
    async fn fehler_einer_plattform_blockiert_die_anderen_nicht() {
        let fabrik = Arc::new(FakeStreamInfoFabrik {
            mit_zugang: vec![Platform::Twitch, Platform::Kick],
            setzen_fehler: Some((
                Platform::Twitch,
                ChatFehler::Abgelehnt("Twitch lehnt ab: Tag zu lang".into()),
            )),
            lesen_fehler: None,
            gebaut: std::sync::Mutex::new(Vec::new()),
        });
        let dienst = StreamInfoDienst::new(fabrik);
        let patch = StreamInfoPatch {
            tags: Some(vec!["x".repeat(30)]),
            ..Default::default()
        };
        let gesetzt = dienst.setzen_alle(7, &patch).await;
        assert_eq!(gesetzt.len(), 2);
        assert!(!gesetzt[0].ok);
        assert_eq!(
            gesetzt[0].hinweis.as_deref(),
            Some("Twitch lehnt ab: Tag zu lang")
        );
        assert!(gesetzt[1].ok, "Kick trotzdem gesetzt");
    }
}
