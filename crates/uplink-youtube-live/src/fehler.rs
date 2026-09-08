use uplink_chat::token::TokenFehler;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApiFehler {
    #[error("YouTube verweigert die nötigen Rechte")]
    RechteFehlen,
    #[error("YouTube verlangt eine neue Anmeldung")]
    NeuAnmeldungNoetig,
    #[error("Live ist für diesen Kanal nicht freigeschaltet")]
    LiveNichtFreigeschaltet,
    #[error("YouTube-Kontingent ist erschöpft")]
    Quota,
    #[error("YouTube drosselt die Anfragen gerade")]
    Ratelimit,
    #[error("YouTube kennt die angefragte Ressource nicht")]
    NichtGefunden,
    #[error("YouTube weist die Anfrage als ungültig zurück: {0}")]
    Ungueltig(String),
    #[error("YouTube ist nicht erreichbar: {0}")]
    Transport(String),
    #[error("YouTube hat den Schreibversuch nicht bestätigt")]
    Unklar,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LiveFehler {
    #[error("Eingabe ist ungültig: {0}")]
    Ungueltig(&'static str),
    #[error("Datenbank meldet: {0}")]
    Store(&'static str),
    #[error("Tokenzugang nicht möglich: {0}")]
    Broker(TokenFehler),
    #[error("Ein paralleler Vorgang läuft für diesen Nutzer bereits")]
    Belegt,
}
