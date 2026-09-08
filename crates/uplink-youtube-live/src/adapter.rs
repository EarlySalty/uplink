use crate::api::{BroadcastWunsch, LiveApi, StreamRessource};
use crate::fehler::{ApiFehler, LiveFehler};
use crate::model::{
    Blockgrund, Endegrund, Identitaet, IngestZugang, Referenzen, RunAnforderung, RunZustand,
    Schritt, Vorbereitung, Zustand,
};
use crate::store::{GespeicherteEinstellungen, Run, RunNeu, RunStore};
use chrono::Utc;
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uplink_chat::Platform;
use uplink_chat::token::{TokenFehler, TokenQuelle};
use zeroize::Zeroizing;

const SCOPE_FORCE: &str = "https://www.googleapis.com/auth/youtube.force-ssl";
const SCOPE_VOLL: &str = "https://www.googleapis.com/auth/youtube";
const AKTIV: &[&str] = &["vorbereitung", "vorbereitet", "sendet", "live"];

enum RufFehler {
    Blockiert(Blockgrund),
    Broker(TokenFehler),
    Api(ApiFehler),
}

enum Abbruch {
    Zustand(Zustand),
    Fehler(LiveFehler),
}

impl From<&'static str> for Abbruch {
    fn from(meldung: &'static str) -> Self {
        Abbruch::Fehler(LiveFehler::Store(meldung))
    }
}

fn block(grund: Blockgrund) -> Abbruch {
    Abbruch::Zustand(Zustand::Blockiert { grund, refs: None })
}

fn refs_opt(run: &Run) -> Option<Referenzen> {
    match (&run.stream_id, &run.broadcast_id) {
        (Some(s), Some(b)) => Some(Referenzen {
            run_id: run.run_id,
            broadcast_id: b.clone(),
            stream_id: s.clone(),
        }),
        _ => None,
    }
}

fn ingest_aus(stream: StreamRessource) -> IngestZugang {
    IngestZugang {
        rtmps_url: stream.rtmps_url.unwrap_or_default(),
        stream_name: stream.stream_name,
    }
}

fn api_zustand(fehler: ApiFehler, refs: Option<Referenzen>, ist_bericht: bool) -> Zustand {
    match fehler {
        ApiFehler::RechteFehlen => Zustand::Blockiert {
            grund: Blockgrund::RechteFehlen,
            refs,
        },
        ApiFehler::NeuAnmeldungNoetig => Zustand::Blockiert {
            grund: Blockgrund::NeuAnmeldungNoetig,
            refs,
        },
        ApiFehler::LiveNichtFreigeschaltet => Zustand::Blockiert {
            grund: Blockgrund::LiveNichtFreigeschaltet,
            refs,
        },
        ApiFehler::Quota if !ist_bericht => Zustand::Blockiert {
            grund: Blockgrund::Quota,
            refs,
        },
        ApiFehler::Quota => Zustand::Fehler {
            refs,
            fehler: ApiFehler::Quota,
            wiederaufnehmbar: true,
        },
        ApiFehler::Ratelimit => Zustand::Fehler {
            refs,
            fehler: ApiFehler::Ratelimit,
            wiederaufnehmbar: true,
        },
        ApiFehler::Transport(meldung) => Zustand::Fehler {
            refs,
            fehler: ApiFehler::Transport(meldung),
            wiederaufnehmbar: true,
        },
        ApiFehler::NichtGefunden => Zustand::Fehler {
            refs,
            fehler: ApiFehler::NichtGefunden,
            wiederaufnehmbar: false,
        },
        ApiFehler::Ungueltig(meldung) => Zustand::Fehler {
            refs,
            fehler: ApiFehler::Ungueltig(meldung),
            wiederaufnehmbar: false,
        },
        ApiFehler::Unklar => Zustand::Fehler {
            refs,
            fehler: ApiFehler::Unklar,
            wiederaufnehmbar: true,
        },
    }
}

fn persistierter_zustand(run: &Run) -> Zustand {
    let refs = refs_opt(run);
    match run.zustand {
        RunZustand::Vorbereitet => refs
            .map(|refs| Zustand::Vorbereitet { refs })
            .unwrap_or(Zustand::Inaktiv),
        RunZustand::Sendet => refs
            .map(|refs| Zustand::Sendet {
                refs,
                stream_status: String::new(),
            })
            .unwrap_or(Zustand::Inaktiv),
        RunZustand::Live => match refs {
            Some(refs) => Zustand::Live {
                refs,
                seit: run.live_seit.unwrap_or_else(Utc::now),
            },
            None => Zustand::Inaktiv,
        },
        RunZustand::Vorbereitung | RunZustand::Beendet => Zustand::Inaktiv,
    }
}

pub struct YouTubeLive {
    api: Arc<dyn LiveApi>,
    store: Arc<dyn RunStore>,
    tokens: Arc<TokenQuelle>,
    sperre: Mutex<HashMap<i64, Arc<tokio::sync::Mutex<()>>>>,
}

impl YouTubeLive {
    pub fn new(api: Arc<dyn LiveApi>, store: Arc<dyn RunStore>, tokens: Arc<TokenQuelle>) -> Self {
        Self {
            api,
            store,
            tokens,
            sperre: Mutex::new(HashMap::new()),
        }
    }

    fn nutzer_sperre(&self, streamer_id: i64) -> Arc<tokio::sync::Mutex<()>> {
        let mut karte = self.sperre.lock().expect("Nutzersperre");
        karte
            .entry(streamer_id)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    async fn token(&self, id: &Identitaet) -> Result<Zeroizing<String>, RufFehler> {
        match self.tokens.zugang(id.streamer_id, Platform::YouTube).await {
            Ok(grant) => {
                if grant.platform_user_id != id.channel_id {
                    return Err(RufFehler::Blockiert(Blockgrund::IdentitaetAbweichung));
                }
                if !grant
                    .scopes
                    .iter()
                    .any(|s| s == SCOPE_FORCE || s == SCOPE_VOLL)
                {
                    return Err(RufFehler::Blockiert(Blockgrund::RechteFehlen));
                }
                Ok(Zeroizing::new(grant.access_token.clone()))
            }
            Err(TokenFehler::NeuAnmeldungNoetig(_)) => {
                Err(RufFehler::Blockiert(Blockgrund::NeuAnmeldungNoetig))
            }
            Err(fehler) => Err(RufFehler::Broker(fehler)),
        }
    }

    async fn ruf<'s, T, F>(&'s self, id: &Identitaet, call: F) -> Result<T, RufFehler>
    where
        F: Fn(Zeroizing<String>) -> BoxFuture<'s, Result<T, ApiFehler>>,
    {
        let token = self.token(id).await?;
        match call(token).await {
            Err(ApiFehler::NeuAnmeldungNoetig) => {
                self.tokens.invalidieren(id.streamer_id, Platform::YouTube);
                let token = self.token(id).await?;
                match call(token).await {
                    Err(ApiFehler::NeuAnmeldungNoetig) => {
                        Err(RufFehler::Blockiert(Blockgrund::NeuAnmeldungNoetig))
                    }
                    Err(fehler) => Err(RufFehler::Api(fehler)),
                    Ok(wert) => Ok(wert),
                }
            }
            Err(fehler) => Err(RufFehler::Api(fehler)),
            Ok(wert) => Ok(wert),
        }
    }

    async fn holen<'s, T, F>(
        &'s self,
        id: &Identitaet,
        refs: Option<Referenzen>,
        ist_bericht: bool,
        call: F,
    ) -> Result<T, Abbruch>
    where
        F: Fn(Zeroizing<String>) -> BoxFuture<'s, Result<T, ApiFehler>>,
    {
        match self.ruf(id, call).await {
            Ok(wert) => Ok(wert),
            Err(RufFehler::Blockiert(grund)) => {
                Err(Abbruch::Zustand(Zustand::Blockiert { grund, refs }))
            }
            Err(RufFehler::Broker(fehler)) => Err(Abbruch::Fehler(LiveFehler::Broker(fehler))),
            Err(RufFehler::Api(fehler)) => {
                Err(Abbruch::Zustand(api_zustand(fehler, refs, ist_bericht)))
            }
        }
    }

    async fn post<'s, T, F>(
        &'s self,
        id: &Identitaet,
        run: &Run,
        schritt: Schritt,
        call: F,
    ) -> Result<T, Abbruch>
    where
        F: Fn(Zeroizing<String>) -> BoxFuture<'s, Result<T, ApiFehler>>,
    {
        self.store
            .schritt_setzen(run.run_id, run.connection_generation, schritt)
            .await?;
        match self.ruf(id, call).await {
            Ok(wert) => {
                self.store
                    .schritt_loeschen(run.run_id, run.connection_generation)
                    .await?;
                Ok(wert)
            }
            Err(RufFehler::Api(ApiFehler::Unklar)) => Err(Abbruch::Zustand(Zustand::Unklar {
                refs: refs_opt(run),
                schritt,
            })),
            Err(RufFehler::Api(fehler)) => {
                let _ = self
                    .store
                    .schritt_loeschen(run.run_id, run.connection_generation)
                    .await;
                Err(Abbruch::Zustand(api_zustand(fehler, refs_opt(run), false)))
            }
            Err(RufFehler::Blockiert(grund)) => {
                let _ = self
                    .store
                    .schritt_loeschen(run.run_id, run.connection_generation)
                    .await;
                Err(Abbruch::Zustand(Zustand::Blockiert {
                    grund,
                    refs: refs_opt(run),
                }))
            }
            Err(RufFehler::Broker(fehler)) => {
                let _ = self
                    .store
                    .schritt_loeschen(run.run_id, run.connection_generation)
                    .await;
                Err(Abbruch::Fehler(LiveFehler::Broker(fehler)))
            }
        }
    }

    async fn run_neu(
        &self,
        id: &Identitaet,
        uplink_session: &str,
        gespeichert: &GespeicherteEinstellungen,
    ) -> Result<Run, Abbruch> {
        let e = &gespeichert.einstellungen;
        let neu = RunNeu {
            streamer_id: id.streamer_id,
            channel_id: &id.channel_id,
            connection_generation: id.connection_generation,
            uplink_session,
            titel: &e.titel,
            sichtbarkeit: e.sichtbarkeit,
            auto_start: e.auto_start,
            auto_stop: e.auto_stop,
            stream_id: gespeichert.stream_id.as_deref(),
        };
        Ok(self.store.run_anlegen(neu).await?)
    }

    async fn ingest_lesen(
        &self,
        id: &Identitaet,
        run: &Run,
    ) -> Result<Option<IngestZugang>, Abbruch> {
        let Some(sid) = run.stream_id.clone() else {
            return Ok(None);
        };
        let stream = self
            .holen(id, refs_opt(run), true, |t| self.api.stream_lesen(&t, &sid))
            .await?;
        Ok(stream.map(ingest_aus))
    }

    async fn abschluss_live(&self, run: &Run) -> Result<Zustand, Abbruch> {
        let refs = refs_opt(run).ok_or(Abbruch::Fehler(LiveFehler::Store("Referenzen fehlen.")))?;
        self.store
            .zustand_setzen(run.run_id, run.connection_generation, AKTIV, "live")
            .await?;
        let seit = run.live_seit.unwrap_or_else(Utc::now);
        self.store.live_seit_setzen(run.run_id, seit).await?;
        Ok(Zustand::Live { refs, seit })
    }

    async fn transition_abgleich(
        &self,
        id: &Identitaet,
        run: &Run,
        schritt: Schritt,
    ) -> Result<Zustand, Abbruch> {
        let refs = refs_opt(run).ok_or(Abbruch::Fehler(LiveFehler::Store("Referenzen fehlen.")))?;
        let bid = refs.broadcast_id.clone();
        let aktuell = self
            .holen(id, Some(refs.clone()), false, |t| {
                self.api.broadcast_lesen(&t, &bid)
            })
            .await?;
        let Some(b) = aktuell else {
            self.store
                .run_schliessen(
                    run.run_id,
                    run.connection_generation,
                    None,
                    Some(false),
                    Some("Broadcast verschwunden"),
                )
                .await?;
            return Ok(Zustand::Fehler {
                refs: Some(refs),
                fehler: ApiFehler::NichtGefunden,
                wiederaufnehmbar: false,
            });
        };
        let leben = b.life_cycle_status.as_deref().unwrap_or("");
        match schritt {
            Schritt::TransitionLive => match leben {
                "live" | "liveStarting" => {
                    self.store
                        .schritt_loeschen(run.run_id, run.connection_generation)
                        .await?;
                    self.abschluss_live(run).await
                }
                "complete" => {
                    self.store
                        .schritt_loeschen(run.run_id, run.connection_generation)
                        .await?;
                    self.store
                        .run_schliessen(
                            run.run_id,
                            run.connection_generation,
                            Some("session_ende_bestaetigt"),
                            Some(true),
                            None,
                        )
                        .await?;
                    Ok(Zustand::Beendet {
                        refs,
                        grund: Endegrund::SessionEndeBestaetigt,
                        youtube_bestaetigt: true,
                    })
                }
                _ => {
                    let neu = self
                        .post(id, run, Schritt::TransitionLive, |t| {
                            self.api.transition(&t, &bid, "live")
                        })
                        .await?;
                    let leben = neu.life_cycle_status.as_deref().unwrap_or("");
                    if matches!(leben, "live" | "liveStarting") {
                        self.abschluss_live(run).await
                    } else {
                        Ok(Zustand::Sendet {
                            refs,
                            stream_status: String::new(),
                        })
                    }
                }
            },
            Schritt::TransitionComplete => {
                if leben == "complete" {
                    self.store
                        .schritt_loeschen(run.run_id, run.connection_generation)
                        .await?;
                    self.store
                        .run_schliessen(
                            run.run_id,
                            run.connection_generation,
                            Some("session_ende_bestaetigt"),
                            Some(true),
                            None,
                        )
                        .await?;
                    Ok(Zustand::Beendet {
                        refs,
                        grund: Endegrund::SessionEndeBestaetigt,
                        youtube_bestaetigt: true,
                    })
                } else {
                    let _ = self
                        .post(id, run, Schritt::TransitionComplete, |t| {
                            self.api.transition(&t, &bid, "complete")
                        })
                        .await?;
                    self.store
                        .run_schliessen(
                            run.run_id,
                            run.connection_generation,
                            Some("session_ende_bestaetigt"),
                            Some(true),
                            None,
                        )
                        .await?;
                    Ok(Zustand::Beendet {
                        refs,
                        grund: Endegrund::SessionEndeBestaetigt,
                        youtube_bestaetigt: true,
                    })
                }
            }
            _ => Err(Abbruch::Fehler(LiveFehler::Store(
                "Schritt passt nicht zum Abgleich.",
            ))),
        }
    }

    async fn berichten(&self, id: &Identitaet, run: &Run) -> Result<Zustand, Abbruch> {
        let (Some(bid), Some(sid)) = (run.broadcast_id.clone(), run.stream_id.clone()) else {
            return Err(Abbruch::Zustand(Zustand::Fehler {
                refs: None,
                fehler: ApiFehler::Ungueltig("Referenzen fehlen".into()),
                wiederaufnehmbar: false,
            }));
        };
        let refs = Referenzen {
            run_id: run.run_id,
            broadcast_id: bid.clone(),
            stream_id: sid.clone(),
        };
        let stream = self
            .holen(id, Some(refs.clone()), true, |t| {
                self.api.stream_lesen(&t, &sid)
            })
            .await?;
        let broadcast = self
            .holen(id, Some(refs.clone()), true, |t| {
                self.api.broadcast_lesen(&t, &bid)
            })
            .await?;
        let Some(b) = broadcast else {
            self.store
                .run_schliessen(
                    run.run_id,
                    run.connection_generation,
                    None,
                    Some(false),
                    Some("Broadcast fehlt"),
                )
                .await?;
            return Ok(Zustand::Fehler {
                refs: Some(refs),
                fehler: ApiFehler::NichtGefunden,
                wiederaufnehmbar: false,
            });
        };
        let stream_status = stream.and_then(|s| s.stream_status).unwrap_or_default();
        let leben = b.life_cycle_status.as_deref().unwrap_or("");
        if leben == "complete" {
            self.store
                .run_schliessen(
                    run.run_id,
                    run.connection_generation,
                    Some("session_ende_bestaetigt"),
                    Some(true),
                    None,
                )
                .await?;
            return Ok(Zustand::Beendet {
                refs,
                grund: Endegrund::SessionEndeBestaetigt,
                youtube_bestaetigt: true,
            });
        }
        if leben == "revoked" {
            self.store
                .run_schliessen(
                    run.run_id,
                    run.connection_generation,
                    None,
                    Some(false),
                    Some("Broadcast widerrufen"),
                )
                .await?;
            return Ok(Zustand::Fehler {
                refs: Some(refs),
                fehler: ApiFehler::Ungueltig("revoked".into()),
                wiederaufnehmbar: false,
            });
        }
        if leben == "live" {
            if b.bound_stream_id.as_deref() == Some(sid.as_str()) {
                self.store
                    .zustand_setzen(run.run_id, run.connection_generation, AKTIV, "live")
                    .await?;
                let seit = run.live_seit.unwrap_or_else(Utc::now);
                self.store.live_seit_setzen(run.run_id, seit).await?;
                return Ok(Zustand::Live { refs, seit });
            }
            return Ok(Zustand::Fehler {
                refs: Some(refs),
                fehler: ApiFehler::Ungueltig("fremde Bindung".into()),
                wiederaufnehmbar: false,
            });
        }
        if stream_status == "active"
            && matches!(leben, "ready" | "liveStarting" | "testing" | "testStarting")
        {
            self.store
                .zustand_setzen(run.run_id, run.connection_generation, AKTIV, "sendet")
                .await?;
            return Ok(Zustand::Sendet {
                refs,
                stream_status,
            });
        }
        Ok(Zustand::Vorbereitet { refs })
    }

    async fn praeparieren(
        &self,
        id: &Identitaet,
        einstellungen: &crate::model::LiveEinstellungen,
        mut run: Run,
    ) -> Result<Vorbereitung, Abbruch> {
        let mut stream_res: Option<StreamRessource> = None;
        if let Some(schritt) = run.schritt {
            match schritt {
                Schritt::StreamInsert => {
                    let seit = run.schritt_seit.unwrap_or_else(Utc::now);
                    let grenze = seit - chrono::Duration::seconds(60);
                    let kandidaten = self
                        .holen(id, refs_opt(&run), false, |t| self.api.streams_eigene(&t))
                        .await?;
                    let mut treffer: Vec<StreamRessource> = kandidaten
                        .into_iter()
                        .filter(|s| s.titel.as_deref() == Some("Uplink"))
                        .filter(|s| s.published_at.map(|p| p >= grenze).unwrap_or(false))
                        .collect();
                    match treffer.len() {
                        1 => {
                            let cand = treffer.remove(0);
                            let sid = cand.id.clone();
                            self.store
                                .referenzen_setzen(run.run_id, Some(&sid), None)
                                .await?;
                            self.store.stream_id_merken(id.streamer_id, &sid).await?;
                            self.store
                                .schritt_loeschen(run.run_id, run.connection_generation)
                                .await?;
                            run.stream_id = Some(sid);
                            stream_res = Some(cand);
                        }
                        0 => {
                            self.store
                                .schritt_loeschen(run.run_id, run.connection_generation)
                                .await?;
                        }
                        _ => {
                            return Err(Abbruch::Zustand(Zustand::Blockiert {
                                grund: Blockgrund::UnklareZuordnung,
                                refs: refs_opt(&run),
                            }));
                        }
                    }
                }
                Schritt::BroadcastInsert => {
                    let seit = run.schritt_seit.unwrap_or_else(Utc::now);
                    let grenze = seit - chrono::Duration::seconds(60);
                    let mut kandidaten = self
                        .holen(id, refs_opt(&run), false, |t| {
                            self.api.broadcasts_eigene(&t, "upcoming")
                        })
                        .await?;
                    let aktive = self
                        .holen(id, refs_opt(&run), false, |t| {
                            self.api.broadcasts_eigene(&t, "active")
                        })
                        .await?;
                    kandidaten.extend(aktive);
                    let treffer: Vec<_> = kandidaten
                        .into_iter()
                        .filter(|b| b.titel.as_deref() == Some(run.titel.as_str()))
                        .filter(|b| b.published_at.map(|p| p >= grenze).unwrap_or(false))
                        .collect();
                    match treffer.len() {
                        1 => {
                            let bid = treffer[0].id.clone();
                            self.store
                                .referenzen_setzen(run.run_id, None, Some(&bid))
                                .await?;
                            self.store
                                .schritt_loeschen(run.run_id, run.connection_generation)
                                .await?;
                            run.broadcast_id = Some(bid);
                        }
                        0 => {
                            self.store
                                .schritt_loeschen(run.run_id, run.connection_generation)
                                .await?;
                        }
                        _ => {
                            return Err(Abbruch::Zustand(Zustand::Blockiert {
                                grund: Blockgrund::UnklareZuordnung,
                                refs: refs_opt(&run),
                            }));
                        }
                    }
                }
                Schritt::Bind => {
                    self.store
                        .schritt_loeschen(run.run_id, run.connection_generation)
                        .await?;
                }
                Schritt::TransitionLive | Schritt::TransitionComplete => {
                    let z = self.transition_abgleich(id, &run, schritt).await?;
                    return Ok(Vorbereitung {
                        zustand: z,
                        ingest: None,
                    });
                }
            }
            run.schritt = None;
        }

        if stream_res.is_none() {
            match run.stream_id.clone() {
                Some(sid) => {
                    let vorhanden = self
                        .holen(id, refs_opt(&run), false, |t| {
                            self.api.stream_lesen(&t, &sid)
                        })
                        .await?;
                    let brauchbar = vorhanden.filter(|s| {
                        s.ingestion_type.as_deref() == Some("rtmp")
                            && s.stream_status.as_deref() != Some("error")
                            && s.channel_id.as_deref() == Some(id.channel_id.as_str())
                    });
                    match brauchbar {
                        Some(s) => stream_res = Some(s),
                        None => stream_res = Some(self.stream_anlegen(id, &mut run).await?),
                    }
                }
                None => stream_res = Some(self.stream_anlegen(id, &mut run).await?),
            }
        }

        let mut broadcast_neu = false;
        if run.broadcast_id.is_none() {
            let wunsch = BroadcastWunsch {
                titel: run.titel.clone(),
                scheduled_start: Utc::now(),
                sichtbarkeit: run.sichtbarkeit,
                auto_start: run.auto_start,
                auto_stop: run.auto_stop,
            };
            let b = self
                .post(id, &run, Schritt::BroadcastInsert, |t| {
                    self.api.broadcast_anlegen(&t, &wunsch)
                })
                .await?;
            let bid = b.id.clone();
            self.store
                .referenzen_setzen(run.run_id, None, Some(&bid))
                .await?;
            run.broadcast_id = Some(bid);
            broadcast_neu = true;
        }

        let bid = run
            .broadcast_id
            .clone()
            .ok_or(Abbruch::Fehler(LiveFehler::Store("Broadcast fehlt.")))?;
        let sid = run
            .stream_id
            .clone()
            .ok_or(Abbruch::Fehler(LiveFehler::Store("Stream fehlt.")))?;

        if broadcast_neu {
            let gebunden = self
                .post(id, &run, Schritt::Bind, |t| self.api.binden(&t, &bid, &sid))
                .await?;
            if gebunden.bound_stream_id.as_deref() != Some(sid.as_str()) {
                return Err(Abbruch::Zustand(Zustand::Fehler {
                    refs: refs_opt(&run),
                    fehler: ApiFehler::Ungueltig("Bindung nicht bestätigt".into()),
                    wiederaufnehmbar: false,
                }));
            }
        } else {
            let aktuell = self
                .holen(id, refs_opt(&run), false, |t| {
                    self.api.broadcast_lesen(&t, &bid)
                })
                .await?;
            let gebunden = aktuell.and_then(|b| b.bound_stream_id);
            if gebunden.as_deref() != Some(sid.as_str()) {
                let neu = self
                    .post(id, &run, Schritt::Bind, |t| self.api.binden(&t, &bid, &sid))
                    .await?;
                if neu.bound_stream_id.as_deref() != Some(sid.as_str()) {
                    return Err(Abbruch::Zustand(Zustand::Fehler {
                        refs: refs_opt(&run),
                        fehler: ApiFehler::Ungueltig("Bindung nicht bestätigt".into()),
                        wiederaufnehmbar: false,
                    }));
                }
            }
        }

        let kontrolle = self
            .holen(id, refs_opt(&run), false, |t| {
                self.api.broadcast_lesen(&t, &bid)
            })
            .await?;
        let Some(b) = kontrolle else {
            return Err(Abbruch::Zustand(Zustand::Fehler {
                refs: refs_opt(&run),
                fehler: ApiFehler::NichtGefunden,
                wiederaufnehmbar: false,
            }));
        };
        let leben = b.life_cycle_status.as_deref().unwrap_or("");
        if !matches!(leben, "created" | "ready")
            || b.bound_stream_id.as_deref() != Some(sid.as_str())
            || b.enable_auto_start != Some(einstellungen.auto_start)
            || b.enable_auto_stop != Some(einstellungen.auto_stop)
        {
            return Err(Abbruch::Zustand(Zustand::Fehler {
                refs: refs_opt(&run),
                fehler: ApiFehler::Ungueltig("Broadcast weicht ab".into()),
                wiederaufnehmbar: false,
            }));
        }

        self.store
            .zustand_setzen(run.run_id, run.connection_generation, AKTIV, "vorbereitet")
            .await?;
        let refs = Referenzen {
            run_id: run.run_id,
            broadcast_id: bid,
            stream_id: sid,
        };
        Ok(Vorbereitung {
            zustand: Zustand::Vorbereitet { refs },
            ingest: stream_res.map(ingest_aus),
        })
    }

    async fn stream_anlegen(
        &self,
        id: &Identitaet,
        run: &mut Run,
    ) -> Result<StreamRessource, Abbruch> {
        let neu = self
            .post(id, run, Schritt::StreamInsert, |t| {
                self.api.stream_anlegen(&t, "Uplink")
            })
            .await?;
        let sid = neu.id.clone();
        self.store
            .referenzen_setzen(run.run_id, Some(&sid), None)
            .await?;
        self.store.stream_id_merken(id.streamer_id, &sid).await?;
        run.stream_id = Some(sid);
        Ok(neu)
    }

    async fn generation_pruefen(
        &self,
        id: &Identitaet,
        run: &Run,
    ) -> Result<Option<Zustand>, Abbruch> {
        if run.connection_generation < id.connection_generation {
            self.store
                .run_schliessen(
                    run.run_id,
                    run.connection_generation,
                    Some("generation_ueberholt"),
                    None,
                    None,
                )
                .await?;
            return Ok(Some(Zustand::Inaktiv));
        }
        if run.connection_generation > id.connection_generation {
            return Ok(Some(Zustand::Blockiert {
                grund: Blockgrund::VeralteteGeneration,
                refs: refs_opt(run),
            }));
        }
        Ok(None)
    }

    async fn prepare_inner(
        &self,
        id: &Identitaet,
        anforderung: RunAnforderung,
    ) -> Result<Vorbereitung, Abbruch> {
        anforderung
            .pruefen()
            .map_err(|m| Abbruch::Fehler(LiveFehler::Ungueltig(m)))?;
        let Some(gespeichert) = self.store.einstellungen_laden(id.streamer_id).await? else {
            return Err(block(Blockgrund::KeineFreigabe));
        };
        if gespeichert.einstellungen.live_freigegeben_at.is_none() {
            return Err(block(Blockgrund::KeineFreigabe));
        }
        if id.connection_generation < gespeichert.einstellungen.connection_generation {
            return Err(block(Blockgrund::VeralteteGeneration));
        }
        if id.connection_generation > gespeichert.einstellungen.connection_generation {
            return Err(block(Blockgrund::KeineFreigabe));
        }
        let run = match self.store.aktiven_run_laden(id.streamer_id).await? {
            Some(run) if run.connection_generation < id.connection_generation => {
                self.store
                    .run_schliessen(
                        run.run_id,
                        run.connection_generation,
                        Some("generation_ueberholt"),
                        None,
                        None,
                    )
                    .await?;
                self.run_neu(id, &anforderung.uplink_session, &gespeichert)
                    .await?
            }
            Some(run) if run.connection_generation > id.connection_generation => {
                return Err(Abbruch::Zustand(Zustand::Blockiert {
                    grund: Blockgrund::VeralteteGeneration,
                    refs: refs_opt(&run),
                }));
            }
            Some(run) => run,
            None => {
                self.run_neu(id, &anforderung.uplink_session, &gespeichert)
                    .await?
            }
        };

        if let Some(schritt) = run.schritt {
            if matches!(
                schritt,
                Schritt::TransitionLive | Schritt::TransitionComplete
            ) {
                let z = self.transition_abgleich(id, &run, schritt).await?;
                return Ok(Vorbereitung {
                    zustand: z,
                    ingest: None,
                });
            }
        } else if matches!(
            run.zustand,
            RunZustand::Vorbereitet | RunZustand::Sendet | RunZustand::Live
        ) {
            let z = self.berichten(id, &run).await?;
            let ingest = match &z {
                Zustand::Vorbereitet { .. } | Zustand::Sendet { .. } | Zustand::Live { .. } => {
                    self.ingest_lesen(id, &run).await?
                }
                _ => None,
            };
            return Ok(Vorbereitung { zustand: z, ingest });
        }

        self.praeparieren(id, &gespeichert.einstellungen, run).await
    }

    async fn status_inner(&self, id: &Identitaet) -> Result<Zustand, Abbruch> {
        let Some(run) = self.store.aktiven_run_laden(id.streamer_id).await? else {
            return Ok(Zustand::Inaktiv);
        };
        if let Some(z) = self.generation_pruefen(id, &run).await? {
            return Ok(z);
        }
        if let Some(schritt) = run.schritt {
            if matches!(
                schritt,
                Schritt::TransitionLive | Schritt::TransitionComplete
            ) {
                return self.transition_abgleich(id, &run, schritt).await;
            }
            let Some(gespeichert) = self.store.einstellungen_laden(id.streamer_id).await? else {
                return Err(block(Blockgrund::KeineFreigabe));
            };
            let v = self
                .praeparieren(id, &gespeichert.einstellungen, run)
                .await?;
            return Ok(v.zustand);
        }
        match run.zustand {
            RunZustand::Vorbereitung => {
                let Some(gespeichert) = self.store.einstellungen_laden(id.streamer_id).await?
                else {
                    return Err(block(Blockgrund::KeineFreigabe));
                };
                let v = self
                    .praeparieren(id, &gespeichert.einstellungen, run)
                    .await?;
                Ok(v.zustand)
            }
            RunZustand::Beendet => Ok(Zustand::Inaktiv),
            _ => self.berichten(id, &run).await,
        }
    }

    async fn start_inner(&self, id: &Identitaet) -> Result<Zustand, Abbruch> {
        let Some(run) = self.store.aktiven_run_laden(id.streamer_id).await? else {
            return Ok(Zustand::Inaktiv);
        };
        if let Some(z) = self.generation_pruefen(id, &run).await? {
            return Ok(z);
        }
        if let Some(schritt) = run.schritt {
            if matches!(
                schritt,
                Schritt::TransitionLive | Schritt::TransitionComplete
            ) {
                return self.transition_abgleich(id, &run, schritt).await;
            }
            let Some(gespeichert) = self.store.einstellungen_laden(id.streamer_id).await? else {
                return Err(block(Blockgrund::KeineFreigabe));
            };
            let v = self
                .praeparieren(id, &gespeichert.einstellungen, run)
                .await?;
            return Ok(v.zustand);
        }
        if run.auto_start {
            return self.berichten(id, &run).await;
        }
        let (Some(bid), Some(sid)) = (run.broadcast_id.clone(), run.stream_id.clone()) else {
            return Ok(persistierter_zustand(&run));
        };
        let refs = Referenzen {
            run_id: run.run_id,
            broadcast_id: bid.clone(),
            stream_id: sid.clone(),
        };
        let stream = self
            .holen(id, Some(refs.clone()), false, |t| {
                self.api.stream_lesen(&t, &sid)
            })
            .await?;
        let stream_status = stream.and_then(|s| s.stream_status).unwrap_or_default();
        if stream_status != "active" {
            return Ok(Zustand::Sendet {
                refs,
                stream_status,
            });
        }
        let _ = self
            .post(id, &run, Schritt::TransitionLive, |t| {
                self.api.transition(&t, &bid, "live")
            })
            .await?;
        self.berichten(id, &run).await
    }

    async fn medien_inner(&self, id: &Identitaet) -> Result<Zustand, Abbruch> {
        let Some(run) = self.store.aktiven_run_laden(id.streamer_id).await? else {
            return Ok(Zustand::Inaktiv);
        };
        if let Some(z) = self.generation_pruefen(id, &run).await? {
            return Ok(z);
        }
        self.store.unterbrochen_vermerken(run.run_id).await?;
        Ok(persistierter_zustand(&run))
    }

    async fn finish_inner(&self, id: &Identitaet, grund: Endegrund) -> Result<Zustand, Abbruch> {
        let Some(run) = self.store.aktiven_run_laden(id.streamer_id).await? else {
            return Ok(Zustand::Inaktiv);
        };
        if let Some(z) = self.generation_pruefen(id, &run).await? {
            return Ok(z);
        }
        let (Some(bid), Some(sid)) = (run.broadcast_id.clone(), run.stream_id.clone()) else {
            self.store
                .run_schliessen(
                    run.run_id,
                    run.connection_generation,
                    Some(grund.as_str()),
                    Some(false),
                    None,
                )
                .await?;
            return Ok(Zustand::Inaktiv);
        };
        let refs = Referenzen {
            run_id: run.run_id,
            broadcast_id: bid.clone(),
            stream_id: sid,
        };
        let broadcast = self
            .holen(id, Some(refs.clone()), false, |t| {
                self.api.broadcast_lesen(&t, &bid)
            })
            .await?;
        let leben = broadcast
            .and_then(|b| b.life_cycle_status)
            .unwrap_or_default();
        match leben.as_str() {
            "complete" => {
                self.store
                    .run_schliessen(
                        run.run_id,
                        run.connection_generation,
                        Some(grund.as_str()),
                        Some(true),
                        None,
                    )
                    .await?;
                Ok(Zustand::Beendet {
                    refs,
                    grund,
                    youtube_bestaetigt: true,
                })
            }
            "live" | "liveStarting" | "testing" | "testStarting" => {
                let _ = self
                    .post(id, &run, Schritt::TransitionComplete, |t| {
                        self.api.transition(&t, &bid, "complete")
                    })
                    .await?;
                self.store
                    .run_schliessen(
                        run.run_id,
                        run.connection_generation,
                        Some(grund.as_str()),
                        Some(true),
                        None,
                    )
                    .await?;
                Ok(Zustand::Beendet {
                    refs,
                    grund,
                    youtube_bestaetigt: true,
                })
            }
            _ => {
                self.store
                    .run_schliessen(
                        run.run_id,
                        run.connection_generation,
                        Some(grund.as_str()),
                        Some(false),
                        None,
                    )
                    .await?;
                Ok(Zustand::Beendet {
                    refs,
                    grund,
                    youtube_bestaetigt: false,
                })
            }
        }
    }

    pub async fn prepare(
        &self,
        id: &Identitaet,
        anforderung: RunAnforderung,
    ) -> Result<Vorbereitung, LiveFehler> {
        let sperre = self.nutzer_sperre(id.streamer_id);
        let _guard = sperre.try_lock().map_err(|_| LiveFehler::Belegt)?;
        match self.prepare_inner(id, anforderung).await {
            Ok(v) => Ok(v),
            Err(Abbruch::Zustand(z)) => Ok(Vorbereitung {
                zustand: z,
                ingest: None,
            }),
            Err(Abbruch::Fehler(f)) => Err(f),
        }
    }

    pub async fn start(&self, id: &Identitaet) -> Result<Zustand, LiveFehler> {
        let sperre = self.nutzer_sperre(id.streamer_id);
        let _guard = sperre.try_lock().map_err(|_| LiveFehler::Belegt)?;
        ausgang(self.start_inner(id).await)
    }

    pub async fn status(&self, id: &Identitaet) -> Result<Zustand, LiveFehler> {
        let sperre = self.nutzer_sperre(id.streamer_id);
        let _guard = sperre.try_lock().map_err(|_| LiveFehler::Belegt)?;
        ausgang(self.status_inner(id).await)
    }

    pub async fn medien_unterbrochen(&self, id: &Identitaet) -> Result<Zustand, LiveFehler> {
        let sperre = self.nutzer_sperre(id.streamer_id);
        let _guard = sperre.try_lock().map_err(|_| LiveFehler::Belegt)?;
        ausgang(self.medien_inner(id).await)
    }

    pub async fn finish(&self, id: &Identitaet, grund: Endegrund) -> Result<Zustand, LiveFehler> {
        let sperre = self.nutzer_sperre(id.streamer_id);
        let _guard = sperre.try_lock().map_err(|_| LiveFehler::Belegt)?;
        ausgang(self.finish_inner(id, grund).await)
    }
}

fn ausgang(ergebnis: Result<Zustand, Abbruch>) -> Result<Zustand, LiveFehler> {
    match ergebnis {
        Ok(z) => Ok(z),
        Err(Abbruch::Zustand(z)) => Ok(z),
        Err(Abbruch::Fehler(f)) => Err(f),
    }
}
