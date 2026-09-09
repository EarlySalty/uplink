# Unabhängiger Rust-Review: Twitch ohne Lastmessung

Stand: 9. September 2026. Basis: `origin/main` = `4b817d8a8c1039a58eb87384626a06175ddc0acd`. Geprüft wurden die Änderungen in `crates/uplink-service/src/media.rs`, `crates/uplink-service/src/registry.rs`, `crates/uplink-service/src/api.rs` und `crates/uplink-service/tests/normalbetrieb.rs`. Kein Produktcode durch den Reviewer verändert.

SHA-256 von `git diff origin/main -- crates/uplink-service/src/media.rs crates/uplink-service/src/registry.rs crates/uplink-service/src/api.rs crates/uplink-service/tests/normalbetrieb.rs`: `2074195e5241f53088e4915008310d520ac1eb0776875f5c43fd2c74c778ff12`.

Ergebnis der unabhängigen Prüfung: **Keine CRITICAL-, HIGH- oder BLOCKING-Befunde im geprüften Hotfix-Diff; Rust-Review mit grünen finalen Cargo-Belegen abgeschlossen.** Die Integration setzt weiterhin einen konfliktfreien Stand und das bestehende Workspace-Gate voraus. Dieser Bericht ist kein `D-REVIEW-FREMD.md` und erteilt keine Merge- oder Deployfreigabe.

## Geprüftes Verhalten

- `media.rs:434–449`: Der bestehende reine Loopback-Testpfad bleibt erhalten. Nur die leere Enhanced-Profilliste aktiviert für Produktionstwitch das vorhandene gespeicherte Einzelziel. Hochkant oder ein gespeicherter Ausgabecodec außer H.264 werden sichtbar gesperrt. Das verschobene `DesiredOutput` behält Eigentum an Ziel, geschütztem Zugang, Videowunsch und Audiowahl; kein zusätzlicher Secretclone oder neuer Medienkern.
- Der vorhandene `Graph::mixed` führt DesiredOutputs durch `observed_selected` (`uplink-media/src/graph.rs:461–477`); der H.264-Encoder verwendet `libx264` (`graph.rs:907–920`). AV1 am Eingang bleibt unterstützt. Der Hotfix behauptet keine gemessenen zusätzlichen Qualitätsstufen und verlangt keinen Wechsel der OBS-Einstellungen.
- `media.rs:451–490` und `media.rs:537–549`: Bei nicht leerer Profilliste bleibt der vorhandene GoLive-Pfad einschließlich Broker-/Generationsprüfung erhalten. Die extrahierte Zulassung verlangt einen exakt passenden Profilkey und anschließend unverändert eine positive Kapazitätsfreigabe. Fehler wechseln nicht in den Einzelzielpfad.
- `registry.rs:263–310`: Statusaktualisierung und Entprellungsentscheidung erfolgen unter einem kurzen Registry-Lock; die Journalausgabe erfolgt erst nach dessen Freigabe. Kein Guard wird über ein `await` gehalten, keine neue verschachtelte Lockreihenfolge. Die Entprellung ist über Reconnects wirksam, gleitend auf 300 Sekunden seit dem letzten Auftreten begrenzt und hält höchstens 1024 Einträge. Grundwechsel bleiben sichtbar. Poison-Recovery entspricht dem bereits verwendeten Statuspfad; die Zulassung verwendet weiterhin Fehlerpropagation.
- Der Journaltext enthält eine numerische authentifizierte Nutzer-ID, einen festen Plattformnamen und einen statischen Grund. Keine fremden Brokerantworten, Zieladressen oder Zugangswerte werden neu geloggt. Speicher- und Vergleichsaufwand sind durch die feste Obergrenze begrenzt und liegen im Fehlerpfad.
- `api.rs:615–648`: Konkrete Sperren und Medienfehler behalten Vorrang. Die neue Ausgabenotiz erscheint bei startender beziehungsweise tatsächlich sendender Ausgabe; sie verändert den Versandstatus nicht. `SessionStatus.output_notices` bleibt bei späteren Medienstatusaktualisierungen erhalten.

## Regressionen und Nachweisgrenzen

- Der neue ignorierte Normalbetriebstest verlässt die ausschließlich lokale Twitch-Sonderbehandlung absichtlich über die erweiterte synthetische Hostfreigabe. Mit leerer Profilliste, Kapazität null, AV1/BT.709 und einer AAC-Spur erwartet er echten Versand, die sichtbare Nichtmessungsnotiz und eine weiterhin aktive fehlerfreie Session nach weiteren zwei Sekunden. Er prüft außerdem die Audio-Pakethashes und die erwartete Videoanzahl.
- Das AV1-Material wird ausschließlich in der Testfixture mit fester Argumentliste, begrenzter Laufzeit und `kill_on_drop` auf passende BT.709-Metadaten gebracht. Der finale Nachreview umfasst den Wechsel von `libaom-av1` zu `libsvtav1`, `+global_header`, Preset 11 und `lp=2`; alle Werte bleiben feste Testargumente, Audio wird unverändert kopiert. Produktionscode und Assertions blieben seit dem ersten Review unverändert. Test-`unwrap` bleiben außerhalb des Produktionspfads.
- Die direkte Zulassungsregression prüft gesetztes Profil bei Kapazität null und fehlenden Profilkey getrennt negativ sowie den passenden Key bei vorhandener Kapazität positiv. Der Integrationstest mit gesetztem Profil/Kapazität null und fehlender Generation stoppt bereits an der Generation; er allein wäre kein Nachweis der nachgelagerten Kapazitätssperre.
- Registry-Tests prüfen zwölf Reconnects mit unverändertem Grund, Grund-/Plattformwechsel, Zeitablauf, die Obergrenze von 1024 Einträgen und den Erhalt der Ausgabenotiz.
- Der neue lokale Mediennachweis verwendet 320×180 und 50 Videoframes. Er ist weder eine Lastmessung für 2560×1440@60 noch ein Twitch-Annahmebeweis oder ein erneuter OBS-Livebeweis.

## Ausführungsbelege

Die Hauptsession bündelte die Cargo-Ausführung beim Builder. Die folgenden finalen Logs wurden im unabhängigen Review gelesen; unveränderte Cargo-Vollprüfungen wurden nicht parallel oder redundant wiederholt. Cargo-Pfad: `/home/nathanael/.cargo/bin/cargo`.

- `cargo check --workspace --locked -j 4`: Exit 0, `/tmp/uplink-hotfix-check.log`.
- `cargo clippy --workspace --all-targets --locked -j 4 -- -D warnings`: Exit 0 auf der finalen Fixture, `/tmp/uplink-hotfix-clippy.log`.
- `cargo fmt --all --check`: Exit 0 laut Builderabschluss; `git diff --check` zusätzlich im Reviewer-Nachlauf erfolgreich.
- `cargo test --workspace --locked -j 4`: final Exit 0, 424 bestandene Tests und 64 ignorierte Tests in 35 Suites, `/tmp/uplink-hotfix-workspace-tests.log`. Die neuen Zulassungs-, Entprellungs- und Statustests sind darin ausdrücklich grün.
- Separater Normalbetriebslauf mit `--ignored`: 11/11 bestanden, darunter der neue AV1-Einzelzieltest, `/tmp/uplink-hotfix-normalbetrieb.log`.

Der erste Workspace-Lauf scheiterte laut Builder einmal am vorhandenen Timingtest `cancelled_start_drops_unresponsive_peer_transport` (`uplink-media/tests/pusher.rs:473`, Timeout). Der vollständige unveränderte Neulauf ist grün und enthält diesen Test ausdrücklich als bestanden. Das ist ein offengelegter Timing-Ausreißer; daraus wird keine durch den Hotfix behobene Pusher-Ursache abgeleitet.
