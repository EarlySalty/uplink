# Abnahmematrix und Nachweisstand

Stand: 8. September 2026. Maßgeblich ist Abschnitt 13 des [Produktvertrags v0.3](produktvertrag-v0.3.md). **Der vollständige Neubau ist noch nicht abgenommen.** Die vollständigen Medien-, Konto-, Live- und Kapazitätsabnahmen sind in diesem neuen Repository offen. Nachweise aus `rs-relay` werden nicht ungeprüft übertragen.

## Lokale Grundlage gegenüber Produktabnahme

`uplink-core` und `uplink-cli` bilden den ersten lokalen Entwicklungsschritt: Profile, Audiozuordnung, Encode-Planung und begrenzte komprimierte Puffer sowie TOML-Prüfung. Der CLI-Eingang wird beschrieben, nicht aus OBS gemessen. Geplante Encode-Gruppen sind kein gemessener Encoderbetrieb. Lokale Tests dürfen deshalb nur die von ihnen tatsächlich geprüfte Fachregel als belegt markieren.

24 lokale Tests sowie Formatierung, Clippy und Release-Build wurden nach dem Planungsfix geprüft. Der [Rust-Review](review-rust.md) gibt den Stand `385ea02` für die lokale Grundlage frei; der [Sicherheitsreview](review-security.md) prüft den Grundstand `0c9bc4e`. Spätere Gate- und Codezustände stehen in [EVIDENCE.md](../.tasks/2026-09-08-uplink-neubau/EVIDENCE.md). Ein grüner Build schließt keine Live-Abnahme automatisch.

Zusätzlich belegt der [Offline-Medienversuch](mediennachweis.md) den geprüften Dateiweg mit AV1 bzw. H.264 und zwei AAC-Spuren über FFmpeg 8.1.2. Zeitverschiebung und AAC-Anfangsmetadaten bleiben explizite Einschränkungen. Das ist ein Teilnachweis für die Medienauswahl, kein OBS-/RTMPS- oder Twitch-Audio-Nachweis.

| ID | Umfang / zugehörige Anforderungen | Erforderlicher Nachweis | Status / Abhängigkeit |
| --- | --- | --- | --- |
| A-01 | Reales OBS → RTMPS/E-RTMP; P-03–P-06, P-16 | Freigegebene OBS-/OS-/GPU-Kombinationen, AV1/H.264 und freigegebenes HEVC, lange Session, Header, mehrere Tracks, Start/Reconnect, fehlerhafte/fehlende Tracks | Offen; F6 und Medienbibliotheksnachweis |
| A-02 | Live-/VOD-Audio; P-11 | Unterscheidbare Testsignale in tatsächlicher Twitch-Liveausgabe und entstandenem VOD; zusätzlich Hochkant, Wartebild, Rückwechsel und fehlender Mix | Offen; F4, A-01 und Twitch-Sitzung |
| A-03 | Vier Plattformen; P-10, P-12, P-20 | Twitch, Kick, YouTube und TikTok tatsächlich öffentlich live; Kontoverbindung, Titel/Metadaten und vereinbarte Chat-/Aktivitätsfunktionen | Offen; TikTok-Testaccount vorhanden, Einzelrechte unbelegt |
| A-04 | Twitch Enhanced Broadcasting; P-07, P-11 | HEVC 2560×1440, vereinbarte Leiter und Quer-/Hochkant-Kombination samt Live-/VOD-Audio in einem Test | Offen; Kontofähigkeiten und serverseitige Interoperabilität |
| A-05 | Hochkant; P-13 | Gameplay-/Kamera-Crops, PiP/Stacked, Kamera an/aus, Profile und Wechsel aus nur einem fertigen Eingang; sichtbares korrektes Ergebnis auf Zielplattformen | Offen; Screenshot nur textlich übergeben, tatsächliche Bildreferenz nicht beigefügt |
| A-06 | Gemeinsame Encodes; P-09 | Tatsächlich genau ein Encode je identischem Videoprofil; verschiedene Audioauswahlen ohne weiteren Video-Encode; andere Profile und Mandanten getrennt; langsames Ziel isoliert | Offen; lokale Planungsprüfung ist noch kein Laufzeitnachweis |
| A-07 | Qualität und Uploadnutzen; P-05, P-08 | Gleiches Quellmaterial, H.264 6.000 kbit/s gegen AV1-Versuchspunkte, visuelle Prüfung und Metriken nach Serverencode sowie ergänzend auf Plattform; Video/Gesamtupload/Ein-Upload-Ersparnis getrennt | Offen; keine garantierten 40 Prozent und keine freigegebenen HQ-CBR-Presets |
| A-08 | Störungen und Dauerlast; P-14, P-15 | Paketverlust, Verzögerung, Bandbreitendelle, dauerhaft knapper Upload, Ausfall einzelner Plattform, Tokens/Tracks, Worker-Neustarts, Puffer-/Speicherlimits und stabile Zeitbasis | Offen; Medienweg und definierte Fehlersemantik |
| A-09 | Betrieb und Migration; P-01, P-02, P-14 | Mandantensicht, Reservierung, Node-Drain, Schutz aktiver Streams, Daten-/URL-Erhalt, geprüfte Migration und sichere Rückkehr | Offen; alter Dienst bleibt erforderlich |
| A-10 | Variable Profile; P-07 | Wunschprofil vor Ingest, gemessene Quelle, zulässige/abgelehnte Ausgabe mit Grund, Profilwechsel beim Reconnect; Upscale und Frame-Wiederholung ehrlich gekennzeichnet | Offen; lokale deklarative Beispiele decken nur einen Teil der Fachregeln |
| A-11 | Einstellbares Delay; P-17 | Mehrere freigegebene Werte, gemessener interner Versatz, Zeit-/Bytegrenzen, Reserveleerstand, Wiederanlauf und A/V-Synchronität; getestete Semantik einer gegebenenfalls aktiven Änderung | Offen; F5, keine erfundene feste Spanne |
| A-12 | VOD-Export; P-18, P-19 | Wirklich verfügbare Quelle, richtiger Mix/Layout, autorisierter YouTube-Kanal, Resume bei Abbruch, unklarer Abschluss ohne blinden Doppelupload, Verarbeitung/Sichtbarkeit/Aufräumen | Offen; F1–F4, Uploadrechte und Speichervertrag |
| A-13 | Sicherheit; Abschnitt 12 | Mandantentrennung, Tokenbroker, Limits vor Decode, Zieladressenschutz, TLS, Uploads, Fehlermeldungen ohne Secrets, Parser-Fehlerfälle und Fuzztests | Offen als Gesamtprüfung; lokale Teilprüfungen separat dokumentieren |
| A-14 | Oberfläche/Docks; P-02, P-13 | Bestehende URLs/Funktionen korrekt angebunden, Neustartbeständigkeit, Bedienung mit realen Konten, sichtbare Zustände, Browserprüfung und Screenshots | Offen; Übernahme von Assets ist kein Funktionsnachweis |

## Plattformfunktionen einzeln prüfen

| Plattform | Video / Konto | Chat / Aktionen / Aktivitäten | Besondere Abnahme |
| --- | --- | --- | --- |
| Twitch | Im Neubau offen | Lesen, berechtigtes Schreiben, Emotes/Badges, Löschen, Moderation, Titel/Kategorie und Aktivitäten jeweils nachzuweisen | Enhanced Broadcasting, 1440p, Dual Format, Live-/VOD-Mix |
| YouTube | Im Neubau offen | Autorisierte Empfangs-/Schreib-/Löschwege und Ereignisupdates; Umfang einzeln festhalten | Broadcast tatsächlich live, natives Dual Stream falls freigegeben; Upload unabhängig von Live-Rechten |
| Kick | Im Neubau offen | Empfang, Senden, Moderation, Metadaten und Aktivitäten einzeln nachzuweisen | Zulässiger H.264-/CBR-Ausgang; 1440p-Eingang ist kein Kick-1440p-Ausgang |
| TikTok | Testaccount bestätigt; RTMP-/App-Rechte offen | Lesen, Senden, Moderation, Metadaten und Aktivitäten jeweils offen | Tragfähige autorisierte Integration statt stiller Abhängigkeit von unbestätigtem Verfahren |

Die Funktionsmatrix selbst enthält Vorschläge aus v0.3. Die Existenz einer Funktion oder Berechtigung wird durch diese Tabelle nicht behauptet. Nicht vorhandene Plattformfunktionen erhalten keinen täuschenden generischen Schalter.

## Reproduzierbarer Nachweis

Jeder spätere Testbericht nennt Code-Commit, Testaufbau, Software-/Treiber-/OBS-Versionen, Hardware, autorisierte Testkontoidentität ohne Secrets, Eingang und Zielprofile, Audiozuordnung, Layoutrevision, Störungsbedingungen und Messergebnisse. Private Medien oder Tokens gehören nicht ins Repository; nötige Nachweise werden autorisiert und bereinigt abgelegt oder geschützt referenziert.

Ergebnisse werden als **bestanden**, **fehlgeschlagen**, **nicht ausgeführt** oder **durch konkrete Abhängigkeit blockiert** bezeichnet. „Implementiert“ und „lokal getestet“ sind ergänzende Entwicklungsangaben. Eine noch nicht ausgeführte Abnahme ist nicht bestanden. Fehlende Nutzerentscheidungen blockieren nur davon abhängige Arbeit.

## Archiv-Docks vor Aktivierung prüfen

Die gesicherten Docks werden noch nicht vom neuen Dienst ausgeliefert. Der [Gate-Review](review-gate.md) nennt drei vorhandene UI-Befunde, die vor einer aktiven Anbindung behoben und im Browser nachgeprüft werden müssen:

- **Chat:** Die Antwort auf einen Sendeaufruf darf keinen Entwurf löschen, der während des laufenden Requests neu getippt wurde. Der Sendeknopf darf eine noch laufende Aktion nicht versehentlich erneut ermöglichen.
- **Stream-Infos:** Eine verzögert eintreffende Speicherantwort darf jüngere lokale Änderungen nicht überschreiben.
- **Kanalpunkte:** Auch extern erfüllte oder abgelehnte Einlösungen müssen aus den offenen Karten verschwinden.

Diese Punkte gehören zu A-14. Die Archivübernahme gilt dadurch nicht als fertige Oberfläche; in dieser Arbeitsrunde wird daraus keine ungeprüfte UI-Änderung oder Aktivierung abgeleitet.

## Vor dem Produktivwechsel

Die Übergabe des Dienstes benötigt nachgewiesene Medieninteroperabilität, die erforderlichen Produktfunktionen, sichere Datenmigration, Ressourcenfreigabe und eine erprobte Rückkehr. Darauf folgen Deploy, Neustart der tatsächlich zuständigen User-Unit und Live-Prüfung. Unfertiger Entwicklungsstand wird nicht auf den alten Ingest geschaltet. Die endgültige Entfernung des alten Repositorys darf keine benötigten Daten, Assets, Konfigurationen oder Rückkehrmöglichkeit unbemerkt zerstören.
