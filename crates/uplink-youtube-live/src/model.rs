use crate::fehler::ApiFehler;
use chrono::{DateTime, Utc};
use std::fmt;
use std::str::FromStr;
use zeroize::Zeroizing;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identitaet {
    pub streamer_id: i64,
    pub channel_id: String,
    pub connection_generation: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sichtbarkeit {
    Private,
    Unlisted,
    Public,
}

impl Sichtbarkeit {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Unlisted => "unlisted",
            Self::Public => "public",
        }
    }
}

impl FromStr for Sichtbarkeit {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "private" => Ok(Self::Private),
            "unlisted" => Ok(Self::Unlisted),
            "public" => Ok(Self::Public),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveEinstellungen {
    pub titel: String,
    pub sichtbarkeit: Sichtbarkeit,
    pub auto_start: bool,
    pub auto_stop: bool,
    pub live_freigegeben_at: Option<DateTime<Utc>>,
    pub connection_generation: i64,
}

pub fn titel_pruefen(titel: &str) -> Result<String, &'static str> {
    let getrimmt = titel.trim();
    let laenge = getrimmt.chars().count();
    if laenge == 0 {
        return Err("Titel darf nicht leer sein.");
    }
    if laenge > 100 {
        return Err("Titel darf höchstens 100 Zeichen haben.");
    }
    if getrimmt.chars().any(char::is_control) {
        return Err("Titel darf keine Steuerzeichen enthalten.");
    }
    Ok(getrimmt.to_owned())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunAnforderung {
    pub uplink_session: String,
}

impl RunAnforderung {
    pub fn pruefen(&self) -> Result<(), &'static str> {
        let laenge = self.uplink_session.chars().count();
        if !(1..=128).contains(&laenge) {
            return Err("Sitzungskennung muss zwischen 1 und 128 Zeichen lang sein.");
        }
        if self.uplink_session.chars().any(char::is_control) {
            return Err("Sitzungskennung darf keine Steuerzeichen enthalten.");
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct IngestZugang {
    pub rtmps_url: String,
    pub stream_name: Zeroizing<String>,
}

impl fmt::Debug for IngestZugang {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IngestZugang")
            .field("rtmps_url", &self.rtmps_url)
            .field("stream_name", &"[geschützt]")
            .finish()
    }
}

#[derive(Debug)]
pub struct Vorbereitung {
    pub zustand: Zustand,
    pub ingest: Option<IngestZugang>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Referenzen {
    pub run_id: i64,
    pub broadcast_id: String,
    pub stream_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Zustand {
    Inaktiv,
    Vorbereitet {
        refs: Referenzen,
    },
    Sendet {
        refs: Referenzen,
        stream_status: String,
    },
    Live {
        refs: Referenzen,
        seit: DateTime<Utc>,
    },
    Beendet {
        refs: Referenzen,
        grund: Endegrund,
        youtube_bestaetigt: bool,
    },
    Blockiert {
        grund: Blockgrund,
        refs: Option<Referenzen>,
    },
    Fehler {
        refs: Option<Referenzen>,
        fehler: ApiFehler,
        wiederaufnehmbar: bool,
    },
    Unklar {
        refs: Option<Referenzen>,
        schritt: Schritt,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blockgrund {
    KeineFreigabe,
    RechteFehlen,
    NeuAnmeldungNoetig,
    LiveNichtFreigeschaltet,
    IdentitaetAbweichung,
    VeralteteGeneration,
    UnklareZuordnung,
    Quota,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Schritt {
    StreamInsert,
    BroadcastInsert,
    Bind,
    TransitionLive,
    TransitionComplete,
}

impl Schritt {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::StreamInsert => "stream_insert",
            Self::BroadcastInsert => "broadcast_insert",
            Self::Bind => "bind",
            Self::TransitionLive => "transition_live",
            Self::TransitionComplete => "transition_complete",
        }
    }
}

impl FromStr for Schritt {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "stream_insert" => Ok(Self::StreamInsert),
            "broadcast_insert" => Ok(Self::BroadcastInsert),
            "bind" => Ok(Self::Bind),
            "transition_live" => Ok(Self::TransitionLive),
            "transition_complete" => Ok(Self::TransitionComplete),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endegrund {
    NutzerStop,
    SessionEndeBestaetigt,
    AdminStop,
}

impl Endegrund {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NutzerStop => "nutzer_stop",
            Self::SessionEndeBestaetigt => "session_ende_bestaetigt",
            Self::AdminStop => "admin_stop",
        }
    }
}

impl FromStr for Endegrund {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "nutzer_stop" => Ok(Self::NutzerStop),
            "session_ende_bestaetigt" => Ok(Self::SessionEndeBestaetigt),
            "admin_stop" => Ok(Self::AdminStop),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunZustand {
    Vorbereitung,
    Vorbereitet,
    Sendet,
    Live,
    Beendet,
}

impl RunZustand {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Vorbereitung => "vorbereitung",
            Self::Vorbereitet => "vorbereitet",
            Self::Sendet => "sendet",
            Self::Live => "live",
            Self::Beendet => "beendet",
        }
    }
}

impl FromStr for RunZustand {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "vorbereitung" => Ok(Self::Vorbereitung),
            "vorbereitet" => Ok(Self::Vorbereitet),
            "sendet" => Ok(Self::Sendet),
            "live" => Ok(Self::Live),
            "beendet" => Ok(Self::Beendet),
            _ => Err(()),
        }
    }
}
