# Auftrag: Uplink als sauberes neues Rust-Repository

**Datum:** 8. September 2026. **Grundlage:** übergebener Produktvertrag v0.3 und anschließende ausdrückliche Nutzeranweisung, Uplink in einem neuen Repository neu zu schreiben. Der Nutzer hat nochmals klargestellt: Neuimplementierung im anderen Repository, keine weitere Aufräumrunde im alten Code.

`ORCHESTRIERUNG[OR-1]: Klasse hoch | Phase implement | Artefakt: .tasks/2026-09-08-uplink-neubau`

## Ergebnis des vollständigen Auftrags

Ein unabhängiges Uplink-Repository mit neu geschriebenem Rust-Backend und Medienkern, geprüft weiterverwendeten Oberflächen/Overlays/Docks und dem bestätigten Vier-Plattform-Umfang. Der alte Kern wird nicht übernommen. Neues Repository, Bauen, Prüfen, Merge und Push sind beauftragt. Ein unfertiger lokaler Entwicklungsstand wird nicht als vollständiger produktiver Neubau dargestellt.

## Diese Aufgabenakte

Diese Akte hält die erste Umsetzung und ihre tatsächlichen Nachweise fest: sauberes privates Repository, explizite Architektur und Abnahmematrix, Rust-Grundlage mit gezielten lokalen Tests, nachvollziehbarer Bestandserhalt sowie unabhängiger Review und Gate. Der vollständige Produktumfang bleibt im [Architektur- und Bauplan](../../docs/architektur.md) und in der [Abnahmematrix](../../docs/abnahme.md) offen sichtbar.

## Grenzen und unveränderte Entscheidungen

- Kein alter Medienkern, keine alte Git-Historie, DB, Secrets, Laufzeitkonfiguration oder Build-Artefakte als Neubau importieren.
- Backend Rust; geprüfte Browserassets bleiben zulässig. Keine unnötigen Dienste/Abstraktionsschichten.
- Keine ENV-Konfiguration, keine neue Tokenverwaltung und keine neue Modellabhängigkeit. Vorhandenen autoritativen OAuth-/Tokenbroker nutzen.
- Twitch, Kick, YouTube, TikTok, AV1/H.264, 1440p/Enhanced Broadcasting, Live-/VOD-Audio, Hochkant, variable Profile, Delay und VOD-Upload bleiben im bestätigten Umfang mit den jeweils dokumentierten offenen Details.
- F1–F6 bleiben offen. Insbesondere entscheidet das neue Uplink-Repository nicht über VOD-Quelle, -Repository, Speicherprodukt oder Aufbewahrung.
- Tatsächlicher früherer Layout-Screenshot liegt in diesem Turn nicht vor; die Textbeschreibung wird erhalten, keine visuelle Prüfung behauptet.
- Bestehender Dienst bleibt bis zur geprüften Ablösung erforderlich. Die spätere Löschung des alten Repositorys ist kein in dieser ersten Etappe bereits vollzogener Schritt.

## Abnahmekriterien dieser Arbeitsrunde

1. Neuer eigenständiger Git-Stand mit privaten Remote-Nachweisen; keine pauschale Übernahme der alten Codebasis.
2. Produktvertrag mit 16 Abschnitten, P-01–P-20, F1–F6 und S1–S21 erhalten; neue Repoentscheidung separat dokumentiert.
3. Rust-Workspace baut; lokale Fachregeln und Konfiguration erhalten gezielte Tests. Keine Ports, Encodes oder Plattformfunktionen als fertig ausgeben, wenn nur lokale Planung existiert.
4. Erhaltene Assets und ihre Herkunft/Integrationslücken nachvollziehbar dokumentiert.
5. Unabhängiger kritischer Rust-/Sicherheitsreview und bestehender Workspace-Gate; Befunde beheben und erneut prüfen.
6. Erreichten Stand und fehlende Produktabnahmen ehrlich dokumentieren. Produktivdienst erst nach den dafür notwendigen Nachweisen ersetzen.

## Zuständigkeiten

Die Hauptsession plant, verteilt und überwacht. Agenten führen Bau, Datensammlung und zugewiesene Git-/Remote-/Gate-Schritte aus. Code-Agent besitzt Rust-Workspace und CLI; Bestands-Agent inventarisiert übernehmbare UI und Medienansätze; Dokumentations-Agent besitzt Vertrags-/Architekturdokumente und diese Aufgabenakte. Weitere Prüfer beurteilen unabhängig. Parallele Änderungen und der gemeinsam genutzte Git-Index werden respektiert. Geprüfte Schritte werden einzeln mit nachvollziehbaren Commits auf GitHub gepusht; die Hauptsession koordiniert die Zusammenführung nach `main` und die Branchbereinigung.
