# Zentraler Gate: Ergebnis und verbleibende Prüfpunkte

Stand: 8. September 2026. Dieser Bericht hält die von Hauptsession und ausführendem Agenten gemeldeten Gate-Ergebnisse fest. Er ergänzt den [Rust-Review](review-rust.md) und [Sicherheitsreview](review-security.md), ohne deren Prüfstand umzuschreiben.

| Stand | Ergebnis | Bedeutung |
| --- | --- | --- |
| `0c9bc4e` | Direkter Anlauf `gate_hook.py --review --base main`: Exit 2 ohne Urteil wegen ausgeschöpftem Claude-Wochenkontingent | Kein inhaltliches BLOCK |
| `385ea02` | Reguläre Gate-Kette: **ALLOW** mit vier kleineren Hinweisen | Der eingebaute `gpt-6-astra`-Ausfallpfad übernahm nach dem Kontingentfehler; der Gate wurde nicht verändert |
| `2c4752d` | Erneute reguläre Gate-Kette: **ALLOW**, keine verbleibenden Core-Hinweise | Claude-Kontingentfehler über den unveränderten eingebauten Ausfallpfad behandelt; drei Hinweise zu inaktiven Archiv-Docks bleiben |
| `40ef8fa` | Vollständiger Stand: **ALLOW** | Vier Archiv-Dock-Hinweise bleiben vor Aktivierung offen; der Dateibaum wurde unverändert als `409ae8db498e342a136556efcb3144a15d2e1846` nach `main` übernommen |
| `32653ca` | Isolierter Scuffle-Nachweis: **ALLOW** über den regulären Ausfallpfad | Gate ausdrücklich mit „Repository tools unavailable“; kein unabhängiger Testnachweis. Lokale Prüfungen und nativer Rust-Review sind separat in der Aufgabenakte belegt. Über PR #2 mit identischem Dateibaum als `001724fdacded263f16681ba0aec8e5f348de601` gemergt. |
| `4a5c1b6cfb01af19f33f2a134f411cee7d395929` | Eigener RTMPS-Schlussgate: Exit 0, **ALLOW** über die unveränderte reguläre Kette | Repository-/Testwerkzeuge waren nicht verfügbar; die effektive Vergleichsbasis wurde nicht ausgegeben. Deshalb kein unabhängiger Test- oder belegter vollständiger Basisvergleich. Native Nachreviews sind separat in [Rust-Review](review-rtmps.md) und [Sicherheitsreview](review-security-rtmps.md) belegt. |

Der reguläre Ausfallpfad ist Teil der vorhandenen Entwicklungsprüfung. Er fügt Uplink keine Modellabhängigkeit hinzu und ändert kein Modell in einem Bot oder Dienst.

Auch die native unabhängige Rust-Nachprüfung ist für den Grundlagenstand `2c4752d` abgeschlossen: Der Reviewer prüfte den Drei-Dateien-Diff und gab den Stand nach erneut 24 Tests sowie Formatierung, Clippy und Build frei. Die zwischenzeitliche technische Threadgrenze ist erledigt. Gate und native Nachprüfung dieses Grundlagenstands sind beide grün.

## Bearbeitung der Hinweise

1. **Decoderzählung, korrigiert in `2c4752d`:** Das Feld heißt `shared_video_decode_count`, die CLI nennt „gemeinsam benötigte Video-Decoder“. Der Kommentar grenzt Audioverarbeitung aus. Es wurde keine Planungslogik geändert. Der ausführende Agent meldet 24 Tests, Formatierung, Clippy, Release-Build und das echte CLI-Beispielszenario grün.
2. **Chat-Dock, vor Aktivierung offen:** Eine Sendeantwort kann einen während des Requests neu getippten Entwurf löschen; der Sendeknopf kann während eines noch laufenden Requests wieder aktiv werden.
3. **Stream-Info-Dock, vor Aktivierung offen:** Eine Speicherantwort kann jüngere lokale Änderungen überschreiben.
4. **Kanalpunkte-Dock, vor Aktivierung offen:** Eine extern erledigte Einlösung wird nicht aus den offenen Karten entfernt.
5. **Suche, vor Aktivierung offen:** Eine geleerte Suchanfrage macht offene Requests nicht ungültig. Verspätete Antworten können alte Treffer wieder anzeigen.

Die vier Dock-Punkte betreffen inaktive, unverändert gesicherte Browserassets. Sie werden im Rahmen von [A-14](abnahme.md#archiv-docks-vor-aktivierung-prüfen) vor aktiver Auslieferung behoben und im Browser geprüft. Der Gate-Stand stellt keine Freigabe für aktive Docks, öffentlichen Medien-Ingest oder Produktivwechsel dar.

Die Hinweise am lokalen RTMPS-Eingang, den Kontrollnachrichten, dem ACK-Fensterwechsel und der Probe wurden korrigiert und unabhängig nachgeprüft. Die einzelnen roten/grünen Regressionen und jeweiligen Prüfstände stehen in den nativen Berichten; deren Ausführung wird nicht dem werkzeuglosen Gate zugerechnet.

[PR #1](https://github.com/EarlySalty/uplink/pull/1), [PR #2](https://github.com/EarlySalty/uplink/pull/2) und [PR #3](https://github.com/EarlySalty/uplink/pull/3) sind **MERGED**. PR #3 wurde am `2026-09-08T13:11:11Z` übernommen. Geprüfter funktionaler Main-Stand: `0aca4d9d3ffc6c3a3fe3b8e3dab165ba391a4c0c`, mit bestätigter Baumgleichheit zu `4a5c1b6cfb01af19f33f2a134f411cee7d395929`. Die Rust-Prüfungen [34229828790](https://github.com/EarlySalty/uplink/actions/runs/34229828790) und [34229834034](https://github.com/EarlySalty/uplink/actions/runs/34229834034) sowie GitGuardian auf diesem Featurestand sind **SUCCESS**. Der geprüfte Codebaum wurde unverändert nach `main` übernommen. Featurebranch lokal/remote gelöscht; nur `main`/`origin/main` und ein Hauptworktree verbleiben. Alle drei Bausteine sind abgeschlossen; der vollständige Live-Neubau bleibt offen.

SonarCloud blieb auch auf dem finalen PR-Stand **CANCELLED**. Die konkrete Ursache ist ohne autorisierten Zugriff auf die Analysedetails unbelegt; ein unauthentifiziertes API-404 beweist kein fehlendes Projekt. Dies ist weder eine Sonar-Freigabe noch ein grüner Sonar-Check.
