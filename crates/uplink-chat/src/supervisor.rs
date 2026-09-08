use crate::adapter::ChatFehler;
pub fn hinweis_fuer(e: &ChatFehler) -> String {
    match e {
        ChatFehler::ZugangUnbestaetigt(p) => format!(
            "{}: Chat-Zugang fehlt oder ist nicht bestätigt. Verbindung im Dashboard prüfen.",
            p.anzeige()
        ),
        ChatFehler::NichtUnterstuetzt(p) => {
            format!("{}: Chat-Zugang ist noch nicht freigegeben.", p.anzeige())
        }
        ChatFehler::NichtVerbunden(p) => format!("{} ist nicht verbunden.", p.anzeige()),
        ChatFehler::NeuAnmeldungNoetig(p) => {
            format!("{} im Dashboard erneut verbinden.", p.anzeige())
        }
        ChatFehler::KeinAdapter => "Chat wird verbunden. Bitte gleich erneut versuchen.".into(),
        ChatFehler::InternerZugang => "Die Kontoverwaltung hat den Dienstzugang abgewiesen.".into(),
        ChatFehler::Netz(_) => {
            "Die Plattform ist gerade nicht erreichbar. Bitte gleich erneut versuchen.".into()
        }
        ChatFehler::Verworfen(_) => "Die Plattform hat die Nachricht nicht zugestellt.".into(),
        ChatFehler::Abgelehnt(text) => text.clone(),
    }
}
pub fn hinweis_ohne_chat(e: &ChatFehler) -> String {
    hinweis_fuer(e)
}
