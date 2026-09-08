# Unabhängiger Rust-Review

Stand: 8. September 2026. Aktueller geprüfter Rust-Stand: `2c4752d`.
Erstprüfung des vollständigen neuen Rust-Codes auf `0c9bc4e` gegen den initialen
`main`-Commit `3111cf3`; fachliche Korrektur auf
`385ea023ce93c2517264038091611481945902f0` und abschließende Nachprüfung der
Benennungspräzisierung auf `2c4752d`.

**Urteil: Freigabe für die gelieferte lokale Rust-Grundlage. Keine offenen
kritischen oder hohen Befunde. Der festgestellte fachliche Fehler ist behoben.**
Das ist keine Freigabe des vollständigen Uplink-Produkts und kein Nachweis für
Ingest, Encoderbetrieb, Plattformkonten oder Produktivwechsel.

## Prüfbereich

Alle Rust-Dateien beider Crates wurden gemeinsam mit dem TOML-Datenfluss,
Beispielszenario, Cargo-Konfiguration, CI, README, Produktvertrag v0.3,
Bestandsübernahme und Aufgabenvertrag geprüft. Die neue Git-Historie enthält
keinen importierten alten Rust-Medienkern. Die separat gesicherten Browserassets
sind noch nicht an einen neuen Dienst angeschlossen.

Der Planer verwendet vollständige Videoeigenschaften einschließlich normalisierter
Bildrate, Farbsignal, GOP und Preset-Revision für Encode-Gruppen. Audioauswahl und
Plattformname erzwingen keinen weiteren Video-Encode. Mandant, Session,
Generation, Quellspur und Layoutrevision bleiben Teil der Identität. Getrennte
Audio-Rollen werden ausdrücklich auf vorhandene Tracks abgebildet; fehlender
VOD-Mix fällt nicht still auf Live-Audio zurück.

Die CLI begrenzt Dateien auf 256 KiB, lehnt unbekannte Felder und Profilreferenzen
ab und gibt Parserdiagnosen mit Konfigurationswerten nicht aus. Angezeigte
Ausgangs-IDs sind syntaktisch begrenzt. Normale Konfiguration verwendet keine
Umgebungsvariablen. Es existieren hier keine Secret-, OAuth- oder Netzwerkpfade.

Die Paket-FIFO prüft Sessiongeneration, Track, Byte- und Paketgrenzen,
Ankunftsalter sowie monotone Zeitstempel. Die geprüften Größenberechnungen können
unter diesen Invarianten nicht überlaufen. Rückstau und Paketablauf sind sichtbar.
Der Aufrufer muss Ablauf im Leerlauf und einen gültigen Medieneinstieg nach
Paketverlust später selbst organisieren; README und Typdokumentation nennen diese
Grenze. Die FIFO behauptet weder einen GOP-Cache noch ein fertiges Delay.

## Gefundener und behobener Fehler

**Mittlere Schwere, geschlossen:** In der Erstfassung wurden bekannte
Audio-Zielverbote und nicht verfügbare Layoutrevisionen erst bei vorhandener
Quelle geprüft. Ohne Quelle erschien deshalb ein bereits ausgeschlossenes
Ausgabeprofil als lediglich „Eingang ausstehend“.

Eigener CLI-Nachweis auf `0c9bc4e`: Im Beispielszenario den Abschnitt `[source]`
entfernen und die erlaubten Audioformate leeren. Ergebnis war Exitcode 4 ohne
Audiofehler. Nach Commit `385ea02` liefert dasselbe Szenario Exitcode 3 und die
konkreten Live-/VOD-Audioverbote. Die quellenunabhängigen Prüfungen stehen jetzt
in `crates/uplink-core/src/planner.rs` ab Zeile 214 vor dem Eingangszweig.
Trackverfügbarkeit, Passthrough und Decoderbedarf bleiben quellenabhängig.

Vier zusätzliche Regressionstests sichern Live-/VOD-Zielaudio und Layout jeweils
im Core und über das echte CLI-Binary. Der Fixer meldete vor der Korrektur vier
rote Tests; deren grüner Endstand wurde unabhängig ausgeführt. Der bestehende
Test für einen tatsächlich unbekannten, noch zulässigen Eingang bleibt grün.

## Ausgeführte Prüfungen

Alle Befehle wurden mit `/home/nathanael/.cargo/bin/cargo` ausgeführt:

| Prüfung | Ergebnis |
| --- | --- |
| `cargo check` | Bestanden, vor und nach Korrektur |
| `cargo clippy -- -D warnings` | Bestanden, vor und nach Korrektur |
| `cargo fmt --check` | Bestanden, vor und nach Korrektur |
| `cargo test` | Erstprüfung 20/20; nach Korrektur 24/24, davon 17 Core und 7 CLI |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | Bestanden |
| `cargo build --workspace --release --locked` | Bestanden |
| `git diff --check` | Bestanden |
| Eigenes CLI-Fehlerszenario | Vorher Exit 4, nachher korrekter Exit 3 |

`git diff main...HEAD -- '*.rs'` und der spätere Korrekturdiff wurden geprüft.
Der ergänzende [Sicherheitsreview](review-security.md) dokumentiert die separaten
Secret- und Abhängigkeitsprüfungen.

Die CI enthält Formatierung, Clippy für alle Testziele, Tests, Release-Build und
das reale lokale Planszenario. Dieser Bericht bestätigt lokale Ausführungen;
ein entfernter CI-Lauf und der zentrale Merge-Gate werden von der Hauptsession
separat nachgewiesen. Die Mergefreigabe setzt dort grüne Prüfungen und gelöste
Konflikte voraus.

## Abschließende Nachprüfung auf `2c4752d`

Der Rust-Diff gegenüber `385ea02` betrifft genau drei Dateien:
`crates/uplink-core/src/planner.rs`, `crates/uplink-cli/src/main.rs` und
`crates/uplink-core/tests/contracts.rs`. `shared_decode_count` heißt jetzt
`shared_video_decode_count`; Typdokumentation und CLI nennen ausdrücklich
Video-Decoder. Der Test verwendet denselben neuen Feldnamen. Berechnung,
Kontrollfluss und sonstiges Verhalten bleiben unverändert. Keine neuen Befunde;
die Freigabe für die lokale Grundlage gilt auch für diesen Stand.

`cargo check`, `cargo clippy -- -D warnings`, `cargo fmt --check` und `cargo test`
wurden dafür erneut unabhängig ausgeführt und bestanden. Weiterhin bestehen
24 Tests, davon 17 Core und 7 CLI. Die Hauptsession meldet außerdem den erneuten
zentralen Gate mit `ALLOW` und den erfolgreichen GitHub-CI-Lauf `34218835484`;
diese beiden externen Ergebnisse wurden in dieser Nachprüfung nicht nochmals
abgerufen.

## Grenzen der Freigabe

Fähigkeiten stammen weiterhin aus ausdrücklich deklarierten Szenariodaten.
Syntaxprüfung von Profil/Level ist keine Codec-Interoperabilität. Skalierung,
Farbkonvertierung, Cropqualität, Hardwarefähigkeiten, Audioverarbeitung und
Plattformannahme benötigen echte Mediennachweise, bevor der Plan ausführbar wird.
Ebenso offen bleiben Ressourcenreservierung, Sessionbetrieb, TLS/Ingest,
Hochkant, Delay, Wartebild, Chat-/Kontenanbindung und VOD-Verarbeitung.

Die offenen Produktentscheidungen F1–F6 wurden durch dieses Review nicht gelöst
oder durch eigene Vorgaben ersetzt. Maßgeblich bleibt die
[Abnahmematrix](abnahme.md).
