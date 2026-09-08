# Nachweise dieser Arbeitsrunde

Stand: 8. September 2026. Das neue Repository enthält eine geprüfte lokale Rust-Grundlage und einen begrenzten Offline-Mediennachweis. Der vollständige Uplink-Neubau ist noch nicht live abgenommen.

`ORCHESTRIERUNG[OR-1]: Klasse hoch | Phase implement | Artefakt: .tasks/2026-09-08-uplink-neubau`

| Gegenstand | Beleg / Ergebnis | Aussagegrenze |
| --- | --- | --- |
| Auftrag und Vertrag | Nutzerauftrag für echte Neuimplementierung; `docs/produktvertrag-v0.3.md` mit 16 Abschnitten, P-01–P-20, F1–F6 und S1–S21 erhalten | Historischer Quellenstand unverändert; VOD-Quelle, -Repository, Speicher und Aufbewahrung bleiben offen |
| Rust-Grundlage | `uplink-core` und `uplink-cli`; geprüfter Fix `385ea02`, anschließende eindeutige Video-Decoderbenennung in `2c4752d` | Deklarierter Szenarioplan, kein ausführbarer Mediengraph und kein OBS-Messwert |
| Lokale Prüfung | Nach Fix 24/24 Tests (17 Core, 7 echte CLI), Formatierung, Clippy und Release-Build bestanden; Code-Agent bestätigt diese Prüfungen samt Beispielszenario erneut auf `2c4752d` | Kein Codec-, Plattform- oder Kapazitätsnachweis |
| Rust-Review | [Native Freigabe auch für `2c4752d`](../../docs/review-rust.md): Drei-Dateien-Diff nachgeprüft, erneut 24 Tests sowie Formatierung, Clippy und Build bestanden; keine offenen kritischen/hohen Befunde | Audio-/Layoutfehler bereits in `385ea02` mit vier zuerst roten Regressionstests behoben; `2c4752d` benennt nur den Video-Decoderzähler eindeutig |
| Sicherheitsreview | [Grundstand `0c9bc4e`](../../docs/review-security.md) ohne aktuellen Blocker; Gitleaks und Cargo Audit bestanden | Fix `385ea02` ändert den Planstatus, keine Sicherheitsgrenze; keine Freigabe für zukünftige öffentliche Pfade |
| Zentraler Gate | [Reguläre Kette](../../docs/review-gate.md) meldete ALLOW für `385ea02` und erneut für `2c4752d`; auch native Rust-Nachprüfung auf `2c4752d` abgeschlossen | Eingebauter Ausfallpfad nach Claude-Kontingentfehler, keine Änderung des Gate; keine offenen Core-Hinweise, drei Archiv-Dock-Punkte vor Aktivierung offen |
| Erhaltener Bestand | [Vier Dock-Quellassets](../../docs/bestandsuebernahme.md) mit [Herkunft und Hashes](../../docs/bestandsmanifest.json); Browser-Syntaxprüfung und bereinigter Secret-Scan bestanden | Inaktive Sicherung; Dashboard und Overlay bleiben im Twitch-Bot; alter Medienkern und alte DB/Secrets nicht importiert |
| Medienversuch | [FFmpeg 8.1.2: AV1 bzw. H.264 plus zwei AAC-Spuren über E-FLV](../../docs/mediennachweis.md), identische komprimierte Spurinhalte und lokale Decodierbarkeit; 6.1.1 lehnt Mehrspurversuch ab | Synthetische Dateien; dokumentierte Zeitverschiebung/Skip-Samples-Verluste; kein OBS-/RTMPS-/Plattformnachweis und keine Engineentscheidung |
| Repository/Remote | Privates `EarlySalty/uplink`; `origin/feat/rust-neubau` steht auf `2c4752d6afbf83a53990d93b31478acaf8f38071`; [PR #1](https://github.com/EarlySalty/uplink/pull/1) angelegt | Push und GitHub-Rückmeldung durch Hauptsession live bestätigt. `main` liegt weiter auf Bootstrap `3111cf3`; Merge und Branchbereinigung noch ausstehend. |
| GitHub Actions | [Pushprüfung 34218835484](https://github.com/EarlySalty/uplink/actions/runs/34218835484) und [PR-Prüfung 34218899893](https://github.com/EarlySalty/uplink/actions/runs/34218899893): beide **SUCCESS**; GitGuardian Secret-Scan ebenfalls erfolgreich | Ergebnisse für PR-Head `2c4752d6afbf83a53990d93b31478acaf8f38071` durch Hauptsession bestätigt; PR #1 meldet `mergeState: CLEAN`, ist aber noch nicht gemergt |
| Produktivbetrieb | [Vollständige Abnahmen](../../docs/abnahme.md) offen; bestehender Relay-Dienst weiter erforderlich | Keine neue Live-Umschaltung oder Löschung des alten Repositorys behauptet |
| Screenshot | Nur Textbeschreibung des früheren Layout-Screenshots übergeben | Kein Bildasset und keine pixelgenaue Prüfung verfügbar |

## Prüfbefehle und Korrektur

Der unabhängige Rust-Reviewer führte am korrigierten Stand `385ea02` unter anderem `cargo check`, `cargo fmt --check`, `cargo test`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo build --workspace --release --locked` und `git diff --check` erfolgreich aus. Das eigene CLI-Fehlerszenario lieferte nach Fix korrekt Exit 3 statt zuvor Exit 4. Nach der reinen Umbenennung in `2c4752d` bestätigte der Code-Agent erneut 24 Tests, Formatierung, Clippy, Release-Build und das tatsächliche CLI-Beispielszenario. Anschließend prüfte auch der native unabhängige Rust-Reviewer den Drei-Dateien-Diff und gab `2c4752d` nach erneut 24 Tests sowie Formatierung, Clippy und Build frei; die zwischenzeitliche technische Threadgrenze ist erledigt.

Der Sicherheitsreview dokumentiert `gitleaks git /home/nathanael/repos/uplink --redact --no-banner` und `cargo audit --file Cargo.lock`, beide Exit 0. Diese Prüfungen gelten für ihren jeweils genannten Commit; später hinzukommende Eingabe-, Auth- oder Medienpfade benötigen einen eigenen Nachweis.

Ein erster direkter `gate_hook.py --review --base main` gegen `0c9bc4e` endete ohne Urteil mit Exit 2 wegen ausgeschöpftem Claude-Wochenkontingent. Die spätere reguläre Kette nutzte ihren vorhandenen Ausfallpfad und lieferte ALLOW. Das war kein inhaltliches BLOCK und kein Anlass für eine Gate-Änderung.

## Noch ausstehender Abschluss

Der geprüfte Code ist mit `2c4752d` auf dem Featurebranch gepusht und über PR #1 sichtbar; beide GitHub-Actions-Prüfungen und der GitGuardian-Scan sind erfolgreich, der PR ist konfliktfrei. Die ergänzende Abschlussdokumentation soll als separater Dokuabschnitt folgen. Die Hauptsession koordiniert die Zusammenführung und die Branchbereinigung. Für `main` ist ein zusammengefasster, nachvollziehbarer Commit dieses Bausteins vorgesehen. Endgültiger Main-SHA und Bereinigung werden nach tatsächlicher Ausführung ergänzt.

Die drei UI-Hinweise an den archivierten Docks bleiben vor Aktivierung verbindliche [Prüfpunkte von A-14](../../docs/abnahme.md#archiv-docks-vor-aktivierung-prüfen). Die vollständigen Medien-/Produktabnahmen und offenen Entscheidungen F1–F6 werden durch lokale Freigaben nicht geschlossen. Keine Secrets, Stream-Keys, Plattformtokens oder privaten Medien gehören in diese Akte.
