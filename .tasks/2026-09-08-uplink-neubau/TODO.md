# Arbeitsstand

Stand: 8. September 2026, nach Merge von PR #1–#3. Geprüfter funktionaler Main-Stand: `0aca4d9d3ffc6c3a3fe3b8e3dab165ba391a4c0c`, identischer Baum zum geprüften Featurestand `4a5c1b6`. Grundlagen-, Offline- und lokaler RTMPS-Baustein sind abgeschlossen; der produktive Neubau bleibt offen.

## Erste Arbeitsrunde

- [x] Nutzerauftrag und neues Repository als Ziel festgehalten.
- [x] Produktvertrag v0.3 mit 16 Abschnitten, P-01–P-20, F1–F6 und Quellen S1–S21 erhalten.
- [x] Rust-Neuimplementierung, Bestandserhalt und offene VOD-Repoentscheidung per ADR getrennt.
- [x] Architektur, vollständigen Umsetzungsplan und offene Produktabnahmen dokumentiert.
- [x] Neues privates Remote nachweisen; Bootstrap-`main` `3111cf3` auf `origin` bestätigt.
- [x] Gezielte Bestandsübernahme mit Manifest und dokumentierten Integrationslücken abschließen; vier Docks bleiben inaktiv.
- [x] Rust-Workspace und lokale Konfigurations-/Planungsgrundlage fertigstellen.
- [x] Audio-/Layout-Planungsfehler mit vier Regressionstests beheben (`385ea02`), erneute 24 Tests sowie Format/Clippy/Release bestanden.
- [x] Decoderzählung eindeutig auf Video begrenzen (`2c4752d`); 24 Tests und echtes CLI-Beispiel erneut grün.
- [x] Medienkandidaten und Offline-E-FLV-Versuch samt Grenzen dokumentieren; endgültige Enginewahl bleibt offen.
- [x] Unabhängigen Rust-/Sicherheitsreview abschließen und Planungsbefund nachprüfen; jeweilige Commitgrenzen dokumentiert.
- [x] Bestehenden Workspace-Gate regulär ausführen: ALLOW für `385ea02`, `2c4752d` und vollständigen Stand `40ef8fa`.
- [x] Vier verbleibende Archiv-Dock-Hinweise als Prüfpunkte vor Aktivierung in A-14 festhalten.
- [x] Geprüften Code einzeln auf GitHub pushen: `origin/feat/rust-neubau` auf `2c4752d`, PR #1 angelegt.
- [x] Abschlussdokumentation übernehmen und erfolgreiche [Main-CI](https://github.com/EarlySalty/uplink/actions/runs/34219867812) dokumentieren.
- [x] Abschließenden Stand nach `main` zusammenführen: PR #1 gemergt, lokaler und entfernter SHA `409ae8db498e342a136556efcb3144a15d2e1846`, Dateibaum identisch zu `40ef8fa`.
- [x] Temporären Branch `feat/rust-neubau` lokal und auf GitHub löschen.

## Isolierter Nachweisbaustein

- [x] Portable Scuffle-Testakte und isolierte CI auf `test/ertmp-mediennachweis` bauen und als `32653ca` committen.
- [x] 13 Debug-/13 Release-Tests, Formatierung, Clippy, separates Cargo Audit und Gitleaks prüfen; nativen Rust-Review und Gate-ALLOW mit dokumentierter Aussagegrenze erhalten.
- [x] Kleine künstliche Fixtures behalten und temporäre Medien löschen; H.264-Abhängigkeitsfehler und Begrenzung des Testadapters dokumentieren.
- [x] [PR #2](https://github.com/EarlySalty/uplink/pull/2) mergen: `main` auf `001724fdacded263f16681ba0aec8e5f348de601`, Dateibaum identisch zu `32653ca0a74d3c6a886d0f1a9b700268953153d2` bestätigt.
- [x] Erfolgreiche [Main-CI](https://github.com/EarlySalty/uplink/actions/runs/34221066802) nachweisen und beide Featurebranches lokal/remote löschen; danach blieben zunächst nur `main` und ein Hauptworktree.

## Lokaler RTMPS-Baustein

- [x] Ausführbaren Rust-Eingang mit injizierter TLS-/Autorisierungskonfiguration, frischen Generationen und begrenzten Track-/Eventzuständen bauen.
- [x] 24 Ingesttests und zuletzt neun Beispieltests sowie echte FFmpeg-8-AV1-/H.264-plus-zwei-AAC-Übertragung prüfen; TLS-CA-/Hostname-Negativfälle und numerische URL-Grenze dokumentieren.
- [x] Metadata-Tracklimit und fremde Codec-Revision mit roten Regressionstests beheben und unabhängig nachprüfen; [Nachweis](../../docs/rtmps-nachweis.md).
- [x] Separaten [Sicherheitsreview](../../docs/review-security-rtmps.md) mit 59 gezielten Tests, drei Audits und Gitleaks abschließen.
- [x] Eigene Gate-Prüfung auf `baf240c` ausführen; beide Hinweise zu `finish()`-Abbruch und Eventbudget-Messung mit zuerst roten Regressionen beheben und nachprüfen.
- [x] Weiteren Hinweis aus Gate-ALLOW auf `b6c18f5` beheben: falsche Kontrollnachrichtenlängen werden als Protokollfehler erkannt; vier Regressionen, 82 Vendor-Tests plus Doctest in Debug/Release und Sicherheitsnachreview grün.
- [x] ACK-Fensterwechsel und vollständige Hilfsereignisse je Spur nachprüfen: Vendor zuletzt 84 Tests plus Doctest, Probe neun Tests; unabhängiger echter FFmpeg-Nachlauf Exit 0.
- [x] Rust-CI und GitGuardian für PR-Stand `e914723` getrennt als erfolgreich nachweisen; SonarCloud ist CANCELLED mit nicht zugänglicher Ursache.
- [ ] Autorisierten Zugriff auf die SonarCloud-Analysedetails klären; kein fehlendes Projekt oder bestandener Check aus dem unauthentifizierten 404 ableiten.
- [x] Eigener Schlussgate auf `4a5c1b6`: Exit 0/ALLOW über unveränderte Kette; fehlende Repositorywerkzeuge und nicht ausgegebene Vergleichsbasis dokumentiert.
- [x] Finalen Featurestand pushen; beide Rust-CI-Läufe und GitGuardian auf `4a5c1b6` erfolgreich nachweisen. SonarCloud bleibt separat CANCELLED.
- [x] [PR #3](https://github.com/EarlySalty/uplink/pull/3) am `2026-09-08T13:11:11Z` mergen: funktionaler Stand `0aca4d9`, Dateibaumgleichheit zu `4a5c1b6` geprüft.
- [x] `feat/rtmps-ingest` lokal und remote löschen; nur `main`/`origin/main` und ein Hauptworktree verbleiben.

## Vollständiger Produktumfang

- [ ] Lokal nachgewiesenen E-RTMP-/RTMPS-Eingang mit öffentlichem Broker und Standard-OBS verbinden; echte Live-/VOD-Rollen und freigegebene Profile beweisen (A-01/A-02).
- [ ] Medienengine anhand der benötigten Kombination auswählen und integrieren.
- [ ] Tatsächliche vier Plattformausgänge inklusive Kontorechten, Enhanced Broadcasting und vereinbartem Dual Format bauen (A-03/A-04).
- [ ] Gemeinsame Encodes, unterschiedliche Audioauswahl und isolierte langsame Ziele im Betrieb messen (A-06).
- [ ] Variable gewünschte/gemessene/aktive Profile vollständig anbinden (A-10).
- [ ] Hochkant aus einem fertigen Eingang mit Layouteditor und echter visueller Prüfung abnehmen (A-05/A-14).
- [ ] F4–F6 in den jeweils abhängigen Betriebs- und Freigaberegeln entscheiden; keine Werte erfinden.
- [ ] Einstellbares Delay, Wartebild, Reconnect, Stop und Audio unter Störungen abnehmen (A-08/A-11).
- [ ] OAuth-/Tokenbroker, persistente OBS-Zugänge sowie Chat-/Aktivitätsumfang integrieren und prüfen (A-03/A-13/A-14).
- [ ] F1–F3 entscheiden: VOD-Quelle, Auslöser/Ziel, Aufbewahrung und zuständiger Speicher/Repository.
- [ ] Autorisierten VOD-Export mit Resume, Abschlussabgleich, Verarbeitung, Sichtbarkeit und Aufräumen abnehmen (A-12).
- [ ] Qualität/Uploadnutzen und gemessene Kapazität belegen (A-07/A-08/A-09).
- [ ] Migration und Rückkehr prüfen, produktionsfähigen Stand deployen, Dienst neu starten und live prüfen (A-09).
- [ ] Vor endgültiger Entfernung des Altbestands alle Laufzeitabhängigkeiten und benötigten Daten/Assets klären.
