@AGENTS.md

# Arbeitskontext

Uplink wird in einem neuen Repository vollständig neu implementiert. Der bisherige `rs-relay`-Medienkern wird nicht übernommen. Er dient nur als lesbare Funktionsreferenz; geprüfte Oberflächen, Overlays und Docks dürfen separat erhalten bleiben. Der Auftrag wurde am 8. September 2026 ausdrücklich bestätigt: neuer Code im anderen Repository, keine erneute Überarbeitung des alten Kerns.

## Maßgebliche Dokumente

- [Produktvertrag v0.3](docs/produktvertrag-v0.3.md): vollständiger Umfang, Vorschläge, offene F1–F6 und übergebener Quellenstand.
- [ADR 0001](docs/adr/0001-neues-repository-und-rust-neubau.md): neues Repository und Rust-Neuimplementierung.
- [Architektur und nächste Arbeit](docs/architektur.md): Zuständigkeiten, Invarianten und Baufolge für den vollen Vier-Plattform-Umfang.
- [Abnahmematrix](docs/abnahme.md): lokale Vertragstests getrennt von echten Medien- und Plattformnachweisen.
- [Lokaler RTMPS-Nachweis](docs/rtmps-nachweis.md): ausführbarer Rust-Eingang, FFmpeg-8-Paketerhalt, TLS-Negativfälle und genaue Grenzen.
- [Aufgabenakte](.tasks/2026-09-08-uplink-neubau/CONTRACT.md): autorisierter Auftrag und Nachweise dieser Arbeitsrunde.

## Integrationsgrenzen

OAuth und Plattformtokens bleiben im vorhandenen autoritativen System. Der Twitch-Bot stellt bereits `/twitch/api/v2/internal/platform-token` bereit. Dessen konkrete Rechte, Refresh-Verhalten und Vertrag müssen bei der Anbindung geprüft werden; ein bestehender Endpunkt ist noch kein Integrationsnachweis für diesen Neubau. Zugänge und OBS-URLs müssen Neustarts überstehen. Live-Identitäten werden nach Plattform-ID geprüft, nie nach Anzeigenamen.

Normale Konfiguration liegt in Dateien, Secrets in Infisical. Keine ENV-basierte Konfiguration. Keine neue Tokenkopie für Chat, Medien oder VOD. Die Entscheidung für dieses Uplink-Repository entscheidet ausdrücklich nicht über ein VOD-Repository, Speicherprodukt, Aufzeichnungsquelle oder Aufbewahrungsfrist.

`uplink-ingest` bindet bislang ausschließlich Loopback und nimmt TLS-Konfiguration sowie Autorisierung als Schnittstellen entgegen. Frische Verbindungsgenerationen und begrenzte Track-/Eventzustände entstehen im Server; Wiremetadaten sind kein vollständiges Planner-Profil. Die vorhandene FFmpeg-Probe ist mit `rtmps://localhost` geprüft: beim getesteten FFmpeg-Build validiert eine numerische URL den Zertifikathost nicht zuverlässig. Öffentliche Brokeranbindung, OBS und Plattformausgänge bleiben eigene Abnahmen.

## Betrieb und Qualität

Der bestehende Dienst wird während des Neubaus weiter gebraucht. Vor dessen Ablösung müssen Konfigurationen und Produktfunktionen geprüft übernommen, Migration und Rückkehr erprobt sowie die benötigten Live-Abnahmen belegt sein. Ein vorhandenes `cargo test` ersetzt keinen OBS-, Plattform- oder Kapazitätsnachweis. Der Nutzer beabsichtigt die spätere Löschung des alten Repositorys; deshalb darf das neue Repository keine Laufzeitpfade dorthin voraussetzen.

Der bestehende Relay-Dienst läuft als User-Unit. Den tatsächlich betroffenen Dienstnamen vor einem späteren Produktivwechsel prüfen und danach `systemctl --user restart <service>` mit Live-Prüfung verwenden. Änderungen allein an diesem Entwicklungsrepository begründen noch keinen Neustart des alten Mediendienstes.

Der zentrale Workspace-Gate liegt derzeit unter `/home/nathanael/Documents/.claude/gpt-workers/gate_hook.py`. Seine Abläufe wiederverwenden, keine zusätzlichen gleichartigen Hooks danebenstellen. Rust-Review und Sicherheitsreview für die relevanten Pfade erfolgen vor Merge; der Fixer prüft seine eigene Änderung nach einem Befund erneut.
