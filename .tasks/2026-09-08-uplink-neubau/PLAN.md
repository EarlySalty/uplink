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

`2c4752d6afbf83a53990d93b31478acaf8f38071` ist auf dem Featurebranch gepusht und in [PR #1](https://github.com/EarlySalty/uplink/pull/1) sichtbar. Native Rust-Nachprüfung und zentraler Gate haben diesen Stand freigegeben. [Pushprüfung](https://github.com/EarlySalty/uplink/actions/runs/34218835484), [PR-Prüfung](https://github.com/EarlySalty/uplink/actions/runs/34218899893) und GitGuardian Secret-Scan sind erfolgreich; der PR meldet `CLEAN`. Es folgen der separate Dokucommit, die Zusammenführung dieses Bausteins nach `main` und die Branchbereinigung. Der Main-Merge ist noch nicht erfolgt.
