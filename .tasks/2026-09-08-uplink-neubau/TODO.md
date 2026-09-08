# Arbeitsstand

Stand: 8. September 2026, nach Gate-ALLOW für `2c4752d`. Erledigt wird nur angekreuzt, wenn ein entsprechender Nachweis vorliegt.

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
- [x] Bestehenden Workspace-Gate regulär ausführen: ALLOW für `385ea02` und `2c4752d`.
- [x] Drei verbleibende Archiv-Dock-Hinweise als Prüfpunkte vor Aktivierung in A-14 festhalten.
- [x] Geprüften Code einzeln auf GitHub pushen: `origin/feat/rust-neubau` auf `2c4752d`, PR #1 angelegt.
- [ ] Ergänzende Abschlussdokumentation separat committen/pushen und endgültiges GitHub-Actions-Ergebnis dokumentieren.
- [ ] Abschließenden Stand nach `main` zusammenführen und Remote-SHA nachweisen.
- [ ] Temporären Branch/Worktree nach Abschluss tatsächlich bereinigen.

## Vollständiger Produktumfang

- [ ] E-RTMP-/RTMPS-Eingang, Standard-OBS und getrenntes Audio technisch beweisen (A-01/A-02).
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
