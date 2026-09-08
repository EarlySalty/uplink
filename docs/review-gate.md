# Zentraler Gate: Ergebnis und verbleibende Prüfpunkte

Stand: 8. September 2026. Dieser Bericht hält die von Hauptsession und ausführendem Agenten gemeldeten Gate-Ergebnisse fest. Er ergänzt den [Rust-Review](review-rust.md) und [Sicherheitsreview](review-security.md), ohne deren Prüfstand umzuschreiben.

| Stand | Ergebnis | Bedeutung |
| --- | --- | --- |
| `0c9bc4e` | Direkter Anlauf `gate_hook.py --review --base main`: Exit 2 ohne Urteil wegen ausgeschöpftem Claude-Wochenkontingent | Kein inhaltliches BLOCK |
| `385ea02` | Reguläre Gate-Kette: **ALLOW** mit vier kleineren Hinweisen | Der eingebaute `gpt-6-astra`-Ausfallpfad übernahm nach dem Kontingentfehler; der Gate wurde nicht verändert |
| `2c4752d` | Erneute reguläre Gate-Kette: **ALLOW**, keine verbleibenden Core-Hinweise | Claude-Kontingentfehler über den unveränderten eingebauten Ausfallpfad behandelt; drei Hinweise zu inaktiven Archiv-Docks bleiben |
| `40ef8fa` | Vollständiger Stand: **ALLOW** | Vier Archiv-Dock-Hinweise bleiben vor Aktivierung offen; der Dateibaum wurde unverändert als `409ae8db498e342a136556efcb3144a15d2e1846` nach `main` übernommen |
| `32653ca` | Isolierter Scuffle-Nachweis: **ALLOW** über den regulären Ausfallpfad | Gate ausdrücklich mit „Repository tools unavailable“; kein unabhängiger Testnachweis. Lokale Prüfungen und nativer Rust-Review sind separat in der Aufgabenakte belegt. Über PR #2 mit identischem Dateibaum als `001724fdacded263f16681ba0aec8e5f348de601` gemergt. |

Der reguläre Ausfallpfad ist Teil der vorhandenen Entwicklungsprüfung. Er fügt Uplink keine Modellabhängigkeit hinzu und ändert kein Modell in einem Bot oder Dienst.

Auch die native unabhängige Rust-Nachprüfung ist inzwischen für `2c4752d` abgeschlossen: Der Reviewer prüfte den Drei-Dateien-Diff und gab den Stand nach erneut 24 Tests sowie Formatierung, Clippy und Build frei. Die zwischenzeitliche technische Threadgrenze ist erledigt. Der aktuelle zentrale Gate-Lauf und die native Nachprüfung sind damit beide grün.

## Bearbeitung der Hinweise

1. **Decoderzählung, korrigiert in `2c4752d`:** Das Feld heißt `shared_video_decode_count`, die CLI nennt „gemeinsam benötigte Video-Decoder“. Der Kommentar grenzt Audioverarbeitung aus. Es wurde keine Planungslogik geändert. Der ausführende Agent meldet 24 Tests, Formatierung, Clippy, Release-Build und das echte CLI-Beispielszenario grün.
2. **Chat-Dock, vor Aktivierung offen:** Eine Sendeantwort kann einen während des Requests neu getippten Entwurf löschen; der Sendeknopf kann während eines noch laufenden Requests wieder aktiv werden.
3. **Stream-Info-Dock, vor Aktivierung offen:** Eine Speicherantwort kann jüngere lokale Änderungen überschreiben.
4. **Kanalpunkte-Dock, vor Aktivierung offen:** Eine extern erledigte Einlösung wird nicht aus den offenen Karten entfernt.
5. **Suche, vor Aktivierung offen:** Eine geleerte Suchanfrage macht offene Requests nicht ungültig. Verspätete Antworten können alte Treffer wieder anzeigen.

Die vier Dock-Punkte betreffen inaktive, unverändert gesicherte Browserassets. Sie werden im Rahmen von [A-14](abnahme.md#archiv-docks-vor-aktivierung-prüfen) vor aktiver Auslieferung behoben und im Browser geprüft. Der Gate-Stand stellt keine Freigabe für aktive Docks, Medien-Ingest oder Produktivwechsel dar.

[PR #1](https://github.com/EarlySalty/uplink/pull/1) und [PR #2](https://github.com/EarlySalty/uplink/pull/2) sind **MERGED**. Der geprüfte funktionale Main-Stand nach PR #2 ist `001724fdacded263f16681ba0aec8e5f348de601`, mit bestätigter Baumgleichheit zu `32653ca0a74d3c6a886d0f1a9b700268953153d2`. Die [Main-Prüfung 34221066802](https://github.com/EarlySalty/uplink/actions/runs/34221066802) ist **SUCCESS**; frühere Main-/Push-/PR-Prüfungen und GitGuardian Secret-Scan waren ebenfalls erfolgreich. Beide Featurebranches wurden lokal und auf GitHub gelöscht; nur `main` und ein Hauptworktree bleiben. Grundlagen- und isolierter Nachweisbaustein sind abgeschlossen; der vollständige Live-Neubau bleibt offen.
