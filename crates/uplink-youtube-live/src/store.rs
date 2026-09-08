use crate::model::{
    Endegrund, LiveEinstellungen, RunZustand, Schritt, Sichtbarkeit, titel_pruefen,
};
use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use tokio_postgres::Row;
use tokio_postgres::types::ToSql;

const CAS: &str = "Run wurde zwischenzeitlich geändert.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GespeicherteEinstellungen {
    pub einstellungen: LiveEinstellungen,
    pub channel_id: String,
    pub stream_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Run {
    pub run_id: i64,
    pub streamer_id: i64,
    pub channel_id: String,
    pub connection_generation: i64,
    pub uplink_session: String,
    pub zustand: RunZustand,
    pub schritt: Option<Schritt>,
    pub schritt_seit: Option<DateTime<Utc>>,
    pub stream_id: Option<String>,
    pub broadcast_id: Option<String>,
    pub titel: String,
    pub sichtbarkeit: Sichtbarkeit,
    pub auto_start: bool,
    pub auto_stop: bool,
    pub fehler: Option<String>,
    pub ende_grund: Option<Endegrund>,
    pub youtube_bestaetigt: Option<bool>,
    pub unterbrochen_at: Option<DateTime<Utc>>,
    pub live_seit: Option<DateTime<Utc>>,
    pub start_angefordert_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct RunNeu<'a> {
    pub streamer_id: i64,
    pub channel_id: &'a str,
    pub connection_generation: i64,
    pub uplink_session: &'a str,
    pub titel: &'a str,
    pub sichtbarkeit: Sichtbarkeit,
    pub auto_start: bool,
    pub auto_stop: bool,
    pub stream_id: Option<&'a str>,
}

pub trait SqlZugang: Send + Sync {
    fn query<'a>(
        &'a self,
        sql: &'a str,
        params: &'a [&'a (dyn ToSql + Sync)],
    ) -> BoxFuture<'a, Result<Vec<Row>, &'static str>>;
}

pub trait RunStore: Send + Sync {
    fn einstellungen_laden<'a>(
        &'a self,
        streamer_id: i64,
    ) -> BoxFuture<'a, Result<Option<GespeicherteEinstellungen>, &'static str>>;
    fn einstellungen_speichern<'a>(
        &'a self,
        streamer_id: i64,
        channel_id: &'a str,
        einstellungen: &'a LiveEinstellungen,
    ) -> BoxFuture<'a, Result<(), &'static str>>;
    fn stream_id_merken<'a>(
        &'a self,
        streamer_id: i64,
        generation: i64,
        stream_id: &'a str,
    ) -> BoxFuture<'a, Result<(), &'static str>>;
    fn aktiven_run_laden<'a>(
        &'a self,
        streamer_id: i64,
    ) -> BoxFuture<'a, Result<Option<Run>, &'static str>>;
    fn run_anlegen<'a>(&'a self, neu: RunNeu<'a>) -> BoxFuture<'a, Result<Run, &'static str>>;
    fn schritt_setzen<'a>(
        &'a self,
        run_id: i64,
        generation: i64,
        schritt: Schritt,
    ) -> BoxFuture<'a, Result<(), &'static str>>;
    fn schritt_loeschen<'a>(
        &'a self,
        run_id: i64,
        generation: i64,
    ) -> BoxFuture<'a, Result<(), &'static str>>;
    fn schritt_abschliessen<'a>(
        &'a self,
        run_id: i64,
        generation: i64,
        stream_id: Option<&'a str>,
        broadcast_id: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), &'static str>>;
    fn referenzen_setzen<'a>(
        &'a self,
        run_id: i64,
        generation: i64,
        stream_id: Option<&'a str>,
        broadcast_id: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), &'static str>>;
    fn zustand_setzen<'a>(
        &'a self,
        run_id: i64,
        generation: i64,
        von: &'a [&'a str],
        nach: &'a str,
    ) -> BoxFuture<'a, Result<(), &'static str>>;
    fn start_angefordert_setzen<'a>(
        &'a self,
        run_id: i64,
        generation: i64,
    ) -> BoxFuture<'a, Result<(), &'static str>>;
    fn run_schliessen<'a>(
        &'a self,
        run_id: i64,
        generation: i64,
        ende_grund: Option<&'a str>,
        youtube_bestaetigt: Option<bool>,
        fehler: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), &'static str>>;
    fn unterbrochen_vermerken<'a>(
        &'a self,
        run_id: i64,
        generation: i64,
    ) -> BoxFuture<'a, Result<(), &'static str>>;
    fn live_seit_setzen<'a>(
        &'a self,
        run_id: i64,
        generation: i64,
        seit: DateTime<Utc>,
    ) -> BoxFuture<'a, Result<(), &'static str>>;
}

pub struct PostgresRunStore {
    sql: Arc<dyn SqlZugang>,
}

const RUN_SPALTEN: &str = "run_id, streamer_id, channel_id, connection_generation, uplink_session, \
    zustand, schritt, schritt_seit, stream_id, broadcast_id, titel, sichtbarkeit, auto_start, \
    auto_stop, fehler, ende_grund, youtube_bestaetigt, unterbrochen_at, live_seit, \
    start_angefordert_at";

impl PostgresRunStore {
    pub fn neu(sql: Arc<dyn SqlZugang>) -> Self {
        Self { sql }
    }
}

fn run_aus_row(row: &Row) -> Result<Run, &'static str> {
    let zustand_text: String = row.try_get(5).map_err(|_| "Run-Zeile ist unlesbar.")?;
    let schritt_text: Option<String> = row.try_get(6).map_err(|_| "Run-Zeile ist unlesbar.")?;
    let sichtbarkeit_text: String = row.try_get(11).map_err(|_| "Run-Zeile ist unlesbar.")?;
    let ende_text: Option<String> = row.try_get(15).map_err(|_| "Run-Zeile ist unlesbar.")?;
    Ok(Run {
        run_id: row.try_get(0).map_err(|_| "Run-Zeile ist unlesbar.")?,
        streamer_id: row.try_get(1).map_err(|_| "Run-Zeile ist unlesbar.")?,
        channel_id: row.try_get(2).map_err(|_| "Run-Zeile ist unlesbar.")?,
        connection_generation: row.try_get(3).map_err(|_| "Run-Zeile ist unlesbar.")?,
        uplink_session: row.try_get(4).map_err(|_| "Run-Zeile ist unlesbar.")?,
        zustand: RunZustand::from_str(&zustand_text).map_err(|_| "Run-Zustand ist ungültig.")?,
        schritt: match schritt_text {
            Some(s) => Some(Schritt::from_str(&s).map_err(|_| "Run-Schritt ist ungültig.")?),
            None => None,
        },
        schritt_seit: row.try_get(7).map_err(|_| "Run-Zeile ist unlesbar.")?,
        stream_id: row.try_get(8).map_err(|_| "Run-Zeile ist unlesbar.")?,
        broadcast_id: row.try_get(9).map_err(|_| "Run-Zeile ist unlesbar.")?,
        titel: row.try_get(10).map_err(|_| "Run-Zeile ist unlesbar.")?,
        sichtbarkeit: Sichtbarkeit::from_str(&sichtbarkeit_text)
            .map_err(|_| "Run-Sichtbarkeit ist ungültig.")?,
        auto_start: row.try_get(12).map_err(|_| "Run-Zeile ist unlesbar.")?,
        auto_stop: row.try_get(13).map_err(|_| "Run-Zeile ist unlesbar.")?,
        fehler: row.try_get(14).map_err(|_| "Run-Zeile ist unlesbar.")?,
        ende_grund: match ende_text {
            Some(s) => Endegrund::from_str(&s).ok(),
            None => None,
        },
        youtube_bestaetigt: row.try_get(16).map_err(|_| "Run-Zeile ist unlesbar.")?,
        unterbrochen_at: row.try_get(17).map_err(|_| "Run-Zeile ist unlesbar.")?,
        live_seit: row.try_get(18).map_err(|_| "Run-Zeile ist unlesbar.")?,
        start_angefordert_at: row.try_get(19).map_err(|_| "Run-Zeile ist unlesbar.")?,
    })
}

impl RunStore for PostgresRunStore {
    fn einstellungen_laden<'a>(
        &'a self,
        streamer_id: i64,
    ) -> BoxFuture<'a, Result<Option<GespeicherteEinstellungen>, &'static str>> {
        Box::pin(async move {
            let sql = "SELECT channel_id, connection_generation, titel, sichtbarkeit, auto_start, \
                auto_stop, live_freigegeben_at, stream_id FROM relay.youtube_live_settings \
                WHERE streamer_id=$1";
            let zeilen = self.sql.query(sql, &[&streamer_id]).await?;
            let Some(row) = zeilen.first() else {
                return Ok(None);
            };
            let sichtbarkeit_text: String =
                row.try_get(3).map_err(|_| "Einstellung ist unlesbar.")?;
            Ok(Some(GespeicherteEinstellungen {
                channel_id: row.try_get(0).map_err(|_| "Einstellung ist unlesbar.")?,
                einstellungen: LiveEinstellungen {
                    connection_generation: row
                        .try_get(1)
                        .map_err(|_| "Einstellung ist unlesbar.")?,
                    titel: row.try_get(2).map_err(|_| "Einstellung ist unlesbar.")?,
                    sichtbarkeit: Sichtbarkeit::from_str(&sichtbarkeit_text)
                        .map_err(|_| "Sichtbarkeit ist ungültig.")?,
                    auto_start: row.try_get(4).map_err(|_| "Einstellung ist unlesbar.")?,
                    auto_stop: row.try_get(5).map_err(|_| "Einstellung ist unlesbar.")?,
                    live_freigegeben_at: row.try_get(6).map_err(|_| "Einstellung ist unlesbar.")?,
                },
                stream_id: row.try_get(7).map_err(|_| "Einstellung ist unlesbar.")?,
            }))
        })
    }

    fn einstellungen_speichern<'a>(
        &'a self,
        streamer_id: i64,
        channel_id: &'a str,
        einstellungen: &'a LiveEinstellungen,
    ) -> BoxFuture<'a, Result<(), &'static str>> {
        Box::pin(async move {
            let titel = titel_pruefen(&einstellungen.titel)?;
            let sql = "INSERT INTO relay.youtube_live_settings \
                (streamer_id, channel_id, connection_generation, titel, sichtbarkeit, auto_start, \
                auto_stop, live_freigegeben_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8) \
                ON CONFLICT (streamer_id) DO UPDATE SET channel_id=EXCLUDED.channel_id, \
                connection_generation=EXCLUDED.connection_generation, titel=EXCLUDED.titel, \
                sichtbarkeit=EXCLUDED.sichtbarkeit, auto_start=EXCLUDED.auto_start, \
                auto_stop=EXCLUDED.auto_stop, live_freigegeben_at=EXCLUDED.live_freigegeben_at, \
                updated_at=clock_timestamp()";
            let sichtbarkeit = einstellungen.sichtbarkeit.as_str();
            self.sql
                .query(
                    sql,
                    &[
                        &streamer_id,
                        &channel_id,
                        &einstellungen.connection_generation,
                        &titel,
                        &sichtbarkeit,
                        &einstellungen.auto_start,
                        &einstellungen.auto_stop,
                        &einstellungen.live_freigegeben_at,
                    ],
                )
                .await?;
            Ok(())
        })
    }

    fn stream_id_merken<'a>(
        &'a self,
        streamer_id: i64,
        generation: i64,
        stream_id: &'a str,
    ) -> BoxFuture<'a, Result<(), &'static str>> {
        Box::pin(async move {
            let sql = "UPDATE relay.youtube_live_settings SET stream_id=$3, \
                updated_at=clock_timestamp() WHERE streamer_id=$1 AND connection_generation=$2 \
                RETURNING streamer_id";
            let zeilen = self
                .sql
                .query(sql, &[&streamer_id, &generation, &stream_id])
                .await?;
            if zeilen.is_empty() { Err(CAS) } else { Ok(()) }
        })
    }

    fn aktiven_run_laden<'a>(
        &'a self,
        streamer_id: i64,
    ) -> BoxFuture<'a, Result<Option<Run>, &'static str>> {
        Box::pin(async move {
            let sql = format!(
                "SELECT {RUN_SPALTEN} FROM relay.youtube_live_runs \
                WHERE streamer_id=$1 AND ended_at IS NULL"
            );
            let zeilen = self.sql.query(&sql, &[&streamer_id]).await?;
            match zeilen.first() {
                Some(row) => Ok(Some(run_aus_row(row)?)),
                None => Ok(None),
            }
        })
    }

    fn run_anlegen<'a>(&'a self, neu: RunNeu<'a>) -> BoxFuture<'a, Result<Run, &'static str>> {
        Box::pin(async move {
            let sql = format!(
                "INSERT INTO relay.youtube_live_runs (streamer_id, channel_id, \
                connection_generation, uplink_session, zustand, stream_id, titel, sichtbarkeit, \
                auto_start, auto_stop) VALUES ($1,$2,$3,$4,'vorbereitung',$5,$6,$7,$8,$9) \
                RETURNING {RUN_SPALTEN}"
            );
            let sichtbarkeit = neu.sichtbarkeit.as_str();
            let zeilen = self
                .sql
                .query(
                    &sql,
                    &[
                        &neu.streamer_id,
                        &neu.channel_id,
                        &neu.connection_generation,
                        &neu.uplink_session,
                        &neu.stream_id,
                        &neu.titel,
                        &sichtbarkeit,
                        &neu.auto_start,
                        &neu.auto_stop,
                    ],
                )
                .await?;
            let row = zeilen.first().ok_or("Run konnte nicht angelegt werden.")?;
            run_aus_row(row)
        })
    }

    fn schritt_setzen<'a>(
        &'a self,
        run_id: i64,
        generation: i64,
        schritt: Schritt,
    ) -> BoxFuture<'a, Result<(), &'static str>> {
        Box::pin(async move {
            let sql = "UPDATE relay.youtube_live_runs SET schritt=$3, \
                schritt_seit=clock_timestamp(), updated_at=clock_timestamp() \
                WHERE run_id=$1 AND connection_generation=$2 AND ended_at IS NULL RETURNING run_id";
            let name = schritt.as_str();
            let zeilen = self.sql.query(sql, &[&run_id, &generation, &name]).await?;
            if zeilen.is_empty() { Err(CAS) } else { Ok(()) }
        })
    }

    fn schritt_loeschen<'a>(
        &'a self,
        run_id: i64,
        generation: i64,
    ) -> BoxFuture<'a, Result<(), &'static str>> {
        Box::pin(async move {
            let sql = "UPDATE relay.youtube_live_runs SET schritt=NULL, schritt_seit=NULL, \
                updated_at=clock_timestamp() WHERE run_id=$1 AND connection_generation=$2 \
                AND ended_at IS NULL RETURNING run_id";
            let zeilen = self.sql.query(sql, &[&run_id, &generation]).await?;
            if zeilen.is_empty() { Err(CAS) } else { Ok(()) }
        })
    }

    fn schritt_abschliessen<'a>(
        &'a self,
        run_id: i64,
        generation: i64,
        stream_id: Option<&'a str>,
        broadcast_id: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), &'static str>> {
        Box::pin(async move {
            let sql = "UPDATE relay.youtube_live_runs SET schritt=NULL, schritt_seit=NULL, \
                stream_id=COALESCE($3,stream_id), broadcast_id=COALESCE($4,broadcast_id), \
                updated_at=clock_timestamp() WHERE run_id=$1 AND connection_generation=$2 \
                AND ended_at IS NULL RETURNING run_id";
            let zeilen = self
                .sql
                .query(sql, &[&run_id, &generation, &stream_id, &broadcast_id])
                .await?;
            if zeilen.is_empty() { Err(CAS) } else { Ok(()) }
        })
    }

    fn referenzen_setzen<'a>(
        &'a self,
        run_id: i64,
        generation: i64,
        stream_id: Option<&'a str>,
        broadcast_id: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), &'static str>> {
        Box::pin(async move {
            let sql = "UPDATE relay.youtube_live_runs SET stream_id=COALESCE($3,stream_id), \
                broadcast_id=COALESCE($4,broadcast_id), updated_at=clock_timestamp() \
                WHERE run_id=$1 AND connection_generation=$2 AND ended_at IS NULL RETURNING run_id";
            let zeilen = self
                .sql
                .query(sql, &[&run_id, &generation, &stream_id, &broadcast_id])
                .await?;
            if zeilen.is_empty() { Err(CAS) } else { Ok(()) }
        })
    }

    fn zustand_setzen<'a>(
        &'a self,
        run_id: i64,
        generation: i64,
        von: &'a [&'a str],
        nach: &'a str,
    ) -> BoxFuture<'a, Result<(), &'static str>> {
        Box::pin(async move {
            let sql = "UPDATE relay.youtube_live_runs SET zustand=$3, updated_at=clock_timestamp() \
                WHERE run_id=$1 AND connection_generation=$2 AND ended_at IS NULL \
                AND zustand = ANY($4) RETURNING run_id";
            let zeilen = self
                .sql
                .query(sql, &[&run_id, &generation, &nach, &von])
                .await?;
            if zeilen.is_empty() { Err(CAS) } else { Ok(()) }
        })
    }

    fn start_angefordert_setzen<'a>(
        &'a self,
        run_id: i64,
        generation: i64,
    ) -> BoxFuture<'a, Result<(), &'static str>> {
        Box::pin(async move {
            let sql = "UPDATE relay.youtube_live_runs SET \
                start_angefordert_at=COALESCE(start_angefordert_at, clock_timestamp()), \
                updated_at=clock_timestamp() WHERE run_id=$1 AND connection_generation=$2 \
                AND ended_at IS NULL RETURNING run_id";
            let zeilen = self.sql.query(sql, &[&run_id, &generation]).await?;
            if zeilen.is_empty() { Err(CAS) } else { Ok(()) }
        })
    }

    fn run_schliessen<'a>(
        &'a self,
        run_id: i64,
        generation: i64,
        ende_grund: Option<&'a str>,
        youtube_bestaetigt: Option<bool>,
        fehler: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), &'static str>> {
        Box::pin(async move {
            let sql = "UPDATE relay.youtube_live_runs SET zustand='beendet', \
                ended_at=clock_timestamp(), ende_grund=$3, youtube_bestaetigt=$4, fehler=$5, \
                updated_at=clock_timestamp() WHERE run_id=$1 AND connection_generation=$2 \
                AND ended_at IS NULL RETURNING run_id";
            let zeilen = self
                .sql
                .query(
                    sql,
                    &[
                        &run_id,
                        &generation,
                        &ende_grund,
                        &youtube_bestaetigt,
                        &fehler,
                    ],
                )
                .await?;
            if zeilen.is_empty() { Err(CAS) } else { Ok(()) }
        })
    }

    fn unterbrochen_vermerken<'a>(
        &'a self,
        run_id: i64,
        generation: i64,
    ) -> BoxFuture<'a, Result<(), &'static str>> {
        Box::pin(async move {
            let sql = "UPDATE relay.youtube_live_runs SET unterbrochen_at=clock_timestamp(), \
                updated_at=clock_timestamp() WHERE run_id=$1 AND connection_generation=$2 \
                AND ended_at IS NULL RETURNING run_id";
            let zeilen = self.sql.query(sql, &[&run_id, &generation]).await?;
            if zeilen.is_empty() { Err(CAS) } else { Ok(()) }
        })
    }

    fn live_seit_setzen<'a>(
        &'a self,
        run_id: i64,
        generation: i64,
        seit: DateTime<Utc>,
    ) -> BoxFuture<'a, Result<(), &'static str>> {
        Box::pin(async move {
            let sql = "UPDATE relay.youtube_live_runs SET live_seit=COALESCE(live_seit,$3), \
                updated_at=clock_timestamp() WHERE run_id=$1 AND connection_generation=$2 \
                AND ended_at IS NULL RETURNING live_seit";
            let zeilen = self.sql.query(sql, &[&run_id, &generation, &seit]).await?;
            if zeilen.is_empty() { Err(CAS) } else { Ok(()) }
        })
    }
}

struct SettingsZeile {
    channel_id: String,
    einstellungen: LiveEinstellungen,
    stream_id: Option<String>,
}

struct RunZeile {
    run: Run,
    beendet: bool,
}

#[derive(Default)]
struct SpeicherInner {
    naechste_id: i64,
    settings: HashMap<i64, SettingsZeile>,
    runs: Vec<RunZeile>,
}

#[derive(Default)]
pub struct SpeicherRunStore {
    inner: Mutex<SpeicherInner>,
}

impl SpeicherRunStore {
    pub fn neu() -> Self {
        Self::default()
    }

    fn treffer(runs: &mut [RunZeile], run_id: i64, generation: i64) -> Option<&mut RunZeile> {
        runs.iter_mut().find(|z| {
            !z.beendet && z.run.run_id == run_id && z.run.connection_generation == generation
        })
    }
}

impl RunStore for SpeicherRunStore {
    fn einstellungen_laden<'a>(
        &'a self,
        streamer_id: i64,
    ) -> BoxFuture<'a, Result<Option<GespeicherteEinstellungen>, &'static str>> {
        Box::pin(async move {
            let inner = self.inner.lock().expect("Speicher");
            Ok(inner
                .settings
                .get(&streamer_id)
                .map(|s| GespeicherteEinstellungen {
                    einstellungen: s.einstellungen.clone(),
                    channel_id: s.channel_id.clone(),
                    stream_id: s.stream_id.clone(),
                }))
        })
    }

    fn einstellungen_speichern<'a>(
        &'a self,
        streamer_id: i64,
        channel_id: &'a str,
        einstellungen: &'a LiveEinstellungen,
    ) -> BoxFuture<'a, Result<(), &'static str>> {
        Box::pin(async move {
            titel_pruefen(&einstellungen.titel)?;
            let mut inner = self.inner.lock().expect("Speicher");
            let vorher = inner
                .settings
                .get(&streamer_id)
                .and_then(|s| s.stream_id.clone());
            inner.settings.insert(
                streamer_id,
                SettingsZeile {
                    channel_id: channel_id.to_owned(),
                    einstellungen: einstellungen.clone(),
                    stream_id: vorher,
                },
            );
            Ok(())
        })
    }

    fn stream_id_merken<'a>(
        &'a self,
        streamer_id: i64,
        generation: i64,
        stream_id: &'a str,
    ) -> BoxFuture<'a, Result<(), &'static str>> {
        Box::pin(async move {
            let mut inner = self.inner.lock().expect("Speicher");
            match inner.settings.get_mut(&streamer_id) {
                Some(zeile) if zeile.einstellungen.connection_generation == generation => {
                    zeile.stream_id = Some(stream_id.to_owned());
                    Ok(())
                }
                _ => Err(CAS),
            }
        })
    }

    fn aktiven_run_laden<'a>(
        &'a self,
        streamer_id: i64,
    ) -> BoxFuture<'a, Result<Option<Run>, &'static str>> {
        Box::pin(async move {
            let inner = self.inner.lock().expect("Speicher");
            Ok(inner
                .runs
                .iter()
                .find(|z| !z.beendet && z.run.streamer_id == streamer_id)
                .map(|z| z.run.clone()))
        })
    }

    fn run_anlegen<'a>(&'a self, neu: RunNeu<'a>) -> BoxFuture<'a, Result<Run, &'static str>> {
        Box::pin(async move {
            let mut inner = self.inner.lock().expect("Speicher");
            if inner
                .runs
                .iter()
                .any(|z| !z.beendet && z.run.streamer_id == neu.streamer_id)
            {
                return Err("Es läuft bereits ein aktiver Run.");
            }
            inner.naechste_id += 1;
            let run = Run {
                run_id: inner.naechste_id,
                streamer_id: neu.streamer_id,
                channel_id: neu.channel_id.to_owned(),
                connection_generation: neu.connection_generation,
                uplink_session: neu.uplink_session.to_owned(),
                zustand: RunZustand::Vorbereitung,
                schritt: None,
                schritt_seit: None,
                stream_id: neu.stream_id.map(str::to_owned),
                broadcast_id: None,
                titel: neu.titel.to_owned(),
                sichtbarkeit: neu.sichtbarkeit,
                auto_start: neu.auto_start,
                auto_stop: neu.auto_stop,
                fehler: None,
                ende_grund: None,
                youtube_bestaetigt: None,
                unterbrochen_at: None,
                live_seit: None,
                start_angefordert_at: None,
            };
            inner.runs.push(RunZeile {
                run: run.clone(),
                beendet: false,
            });
            Ok(run)
        })
    }

    fn schritt_setzen<'a>(
        &'a self,
        run_id: i64,
        generation: i64,
        schritt: Schritt,
    ) -> BoxFuture<'a, Result<(), &'static str>> {
        Box::pin(async move {
            let mut inner = self.inner.lock().expect("Speicher");
            match Self::treffer(&mut inner.runs, run_id, generation) {
                Some(z) => {
                    z.run.schritt = Some(schritt);
                    z.run.schritt_seit = Some(Utc::now());
                    Ok(())
                }
                None => Err(CAS),
            }
        })
    }

    fn schritt_loeschen<'a>(
        &'a self,
        run_id: i64,
        generation: i64,
    ) -> BoxFuture<'a, Result<(), &'static str>> {
        Box::pin(async move {
            let mut inner = self.inner.lock().expect("Speicher");
            match Self::treffer(&mut inner.runs, run_id, generation) {
                Some(z) => {
                    z.run.schritt = None;
                    z.run.schritt_seit = None;
                    Ok(())
                }
                None => Err(CAS),
            }
        })
    }

    fn schritt_abschliessen<'a>(
        &'a self,
        run_id: i64,
        generation: i64,
        stream_id: Option<&'a str>,
        broadcast_id: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), &'static str>> {
        Box::pin(async move {
            let mut inner = self.inner.lock().expect("Speicher");
            match Self::treffer(&mut inner.runs, run_id, generation) {
                Some(z) => {
                    z.run.schritt = None;
                    z.run.schritt_seit = None;
                    if let Some(s) = stream_id {
                        z.run.stream_id = Some(s.to_owned());
                    }
                    if let Some(b) = broadcast_id {
                        z.run.broadcast_id = Some(b.to_owned());
                    }
                    Ok(())
                }
                None => Err(CAS),
            }
        })
    }

    fn referenzen_setzen<'a>(
        &'a self,
        run_id: i64,
        generation: i64,
        stream_id: Option<&'a str>,
        broadcast_id: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), &'static str>> {
        Box::pin(async move {
            let mut inner = self.inner.lock().expect("Speicher");
            match Self::treffer(&mut inner.runs, run_id, generation) {
                Some(z) => {
                    if let Some(s) = stream_id {
                        z.run.stream_id = Some(s.to_owned());
                    }
                    if let Some(b) = broadcast_id {
                        z.run.broadcast_id = Some(b.to_owned());
                    }
                    Ok(())
                }
                None => Err(CAS),
            }
        })
    }

    fn zustand_setzen<'a>(
        &'a self,
        run_id: i64,
        generation: i64,
        von: &'a [&'a str],
        nach: &'a str,
    ) -> BoxFuture<'a, Result<(), &'static str>> {
        Box::pin(async move {
            let ziel = RunZustand::from_str(nach).map_err(|_| "Run-Zustand ist ungültig.")?;
            let mut inner = self.inner.lock().expect("Speicher");
            match Self::treffer(&mut inner.runs, run_id, generation) {
                Some(z) if von.contains(&z.run.zustand.as_str()) => {
                    z.run.zustand = ziel;
                    Ok(())
                }
                _ => Err(CAS),
            }
        })
    }

    fn start_angefordert_setzen<'a>(
        &'a self,
        run_id: i64,
        generation: i64,
    ) -> BoxFuture<'a, Result<(), &'static str>> {
        Box::pin(async move {
            let mut inner = self.inner.lock().expect("Speicher");
            match Self::treffer(&mut inner.runs, run_id, generation) {
                Some(z) => {
                    if z.run.start_angefordert_at.is_none() {
                        z.run.start_angefordert_at = Some(Utc::now());
                    }
                    Ok(())
                }
                None => Err(CAS),
            }
        })
    }

    fn run_schliessen<'a>(
        &'a self,
        run_id: i64,
        generation: i64,
        ende_grund: Option<&'a str>,
        youtube_bestaetigt: Option<bool>,
        fehler: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), &'static str>> {
        Box::pin(async move {
            let mut inner = self.inner.lock().expect("Speicher");
            match Self::treffer(&mut inner.runs, run_id, generation) {
                Some(z) => {
                    z.beendet = true;
                    z.run.zustand = RunZustand::Beendet;
                    z.run.ende_grund = ende_grund.and_then(|g| Endegrund::from_str(g).ok());
                    z.run.youtube_bestaetigt = youtube_bestaetigt;
                    z.run.fehler = fehler.map(str::to_owned);
                    Ok(())
                }
                None => Err(CAS),
            }
        })
    }

    fn unterbrochen_vermerken<'a>(
        &'a self,
        run_id: i64,
        generation: i64,
    ) -> BoxFuture<'a, Result<(), &'static str>> {
        Box::pin(async move {
            let mut inner = self.inner.lock().expect("Speicher");
            match Self::treffer(&mut inner.runs, run_id, generation) {
                Some(z) => {
                    z.run.unterbrochen_at = Some(Utc::now());
                    Ok(())
                }
                None => Err(CAS),
            }
        })
    }

    fn live_seit_setzen<'a>(
        &'a self,
        run_id: i64,
        generation: i64,
        seit: DateTime<Utc>,
    ) -> BoxFuture<'a, Result<(), &'static str>> {
        Box::pin(async move {
            let mut inner = self.inner.lock().expect("Speicher");
            match Self::treffer(&mut inner.runs, run_id, generation) {
                Some(z) => {
                    if z.run.live_seit.is_none() {
                        z.run.live_seit = Some(seit);
                    }
                    Ok(())
                }
                None => Err(CAS),
            }
        })
    }
}
