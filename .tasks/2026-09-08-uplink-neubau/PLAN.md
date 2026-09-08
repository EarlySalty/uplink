# Übernommener Nutzerplan und Baufolge

**Status:** Umsetzung autorisiert. Der [Produktvertrag v0.3](../../docs/produktvertrag-v0.3.md) ist der übernommene fachliche Plan; die neuere Nutzeranweisung bestätigt Neubau und neues Repository. Keine erneute pauschale Freigabe dieses Plans erforderlich.

`ORCHESTRIERUNG[OR-1]: Klasse hoch | Phase implement | Artefakt: .tasks/2026-09-08-uplink-neubau`

1. Neues privates `EarlySalty/uplink` mit eigenständiger Geschichte anlegen.
2. v0.3 mit Evidenzgrenzen erhalten; separate Repo-/Rust-Entscheidung dokumentieren. F1–F6 bleiben davon unberührt.
3. Alten Bestand nur gezielt lesen: erhaltenswerte Oberfläche/Docks und Verträge inventarisieren; alten Medienkern nicht kopieren.
4. Neue minimale Rust-Grundlage bauen: variable Profile, getrennte Audio-Rollen, sessiongebundene Encode-Gruppen, begrenzte komprimierte Puffer und dateibasierte lokale Konfigurationsprüfung.
5. E-RTMP-/Medienkandidaten anhand aktueller Primärquellen und tatsächlicher Vektoren untersuchen; kein voreiliger Engineentscheid.
6. Gezielte Tests ausführen; unabhängigen Rust-/Sicherheitsreview und vorhandenen Workspace-Gate durchlaufen, Befunde selbst nachprüfen.
7. Geprüfte Schritte einzeln mit nachvollziehbarer Commit-Historie auf GitHub pushen. Danach die Arbeitsrunde nach `main` zusammenführen und temporären Branch/Worktree bereinigen. Die Hauptsession koordiniert, zugewiesene Agenten führen aus; kein Force-Push. Nachweise und verbleibende Medien-/Produktabnahmen festhalten.
8. Vollständige Medien-/Plattformimplementation nach [Architektur- und Bauplan](../../docs/architektur.md) fortführen. Migration, Deploy, Neustart und Live-Prüfung gehören zum produktionsfähigen Ergebnis; sie werden nicht auf ein noch unvollständiges Gerüst angewandt.

## Abhängige Entscheidungen

F1–F3 begrenzen die konkrete Aufzeichnung, VOD-Ausführung und Speicherwahl. F4 betrifft fehlenden VOD-Mix und betroffene Ausgänge. F5 betrifft Delay/Stop/Fristen. F6 betrifft garantierte Eingangsmatrix einschließlich HEVC/HDR/OBS. Diese Fragen blockieren keine unabhängige Repositoryanlage, Fachmodellierung oder Kandidatenprüfung.

## Bestätigter Übergabestand

[PR #1](https://github.com/EarlySalty/uplink/pull/1) und [PR #2](https://github.com/EarlySalty/uplink/pull/2) sind gemergt. Der geprüfte funktionale Main-Stand nach PR #2 ist `001724fdacded263f16681ba0aec8e5f348de601`; der Dateibaum ist identisch zum geprüften Stand `32653ca0a74d3c6a886d0f1a9b700268953153d2`. Die [Main-CI](https://github.com/EarlySalty/uplink/actions/runs/34221066802) ist erfolgreich. Die Branches aus PR #1/#2 wurden lokal und auf GitHub gelöscht; anschließend begann der RTMPS-Baustein auf `feat/rtmps-ingest`. Grundlagen- und isolierter Nachweisbaustein sind abgeschlossen; der produktive Neubau bleibt offen.

Der über PR #2 übernommene Scuffle-Baustein enthält eine portable Testakte, kleine synthetische Fixtures und isolierte Debug-/Release-Prüfung in CI. Die Ergebnisse sind ein begrenzter Offline-Nachweis; eine Produktionsengine ist damit nicht freigegeben.

`uplink-ingest` ist als echte lokale Rust-RTMPS-Strecke [nachgewiesen](../../docs/rtmps-nachweis.md) und seit `2026-09-08T13:11:11Z` über [PR #3](https://github.com/EarlySalty/uplink/pull/3) gemergt. Geprüfter funktionaler Main-Stand: `0aca4d9d3ffc6c3a3fe3b8e3dab165ba391a4c0c`; sein Baum entspricht unverändert dem finalen Featurestand `4a5c1b6cfb01af19f33f2a134f411cee7d395929`. Beide PR-Rust-Läufe und GitGuardian sind erfolgreich, der eigene Schlussgate meldet ALLOW mit dokumentierter Aussagegrenze. Featurebranch lokal/remote gelöscht; nur `main`/`origin/main` und ein Hauptworktree verbleiben. Alle drei Bausteine sind gemergt. SonarCloud bleibt ungeklärt; öffentliche Brokeranbindung, Standard-OBS und die vier Plattformausgänge bleiben nächste Arbeiten.
