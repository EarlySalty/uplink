# AV1-2K: kontogebundener Lasttest und Quellhardware

Datum: 18. September 2026. Ausgangsstand: `f2acc17`.

## Auftrag und Änderung

AV1-1440p60 als ausdrücklich gewählten Versuch vom normalen 1080p-Enhanced-Betrieb trennen. Das gespeicherte Hardwareprofil des Streaming-PCs an Twitch weitergeben, aber die tatsächlichen Serverencoder und deren Last nicht mit dieser Quellhardware verwechseln.

Die vorhandene Quellhardware-Übergabe bleibt erhalten. Neue Regressionen vergleichen alle Hardwarefelder, einschließlich einer Liste mit zwei GPUs, sowie einen Twitch-Vertrag mit nur der HEVC-Topspur. `native_2k_av1_test_streamer_id` erlaubt genau eine ausdrückliche Testidentität, ohne die globale Produktionssperre zu entfernen. Die Testreservation ist atomar und exklusiv innerhalb dieses Uplink-Prozesses: keine Verdrängung vorhandener Streams, keine zusätzliche Aufnahme während des Tests, Freigabe beim Sessionende. Keine CPU-Isolation gegenüber anderen Diensten und kein automatischer Last-/Zeitabbruch wurden hinzugefügt.

Zusätzlich wurde ein vorhandener Admission-Fehler korrigiert: 100 zusätzliche AV1-1080p-Einheiten plus eine Basiseinheit konnten nicht in ein Gesamtbudget von 100 passen. Beispiel und Deployment verwenden 99 zusätzliche Einheiten. Der Konfigurationsparser lehnt unerreichbare additive Profilbudgets ab.

## Ausgeführt

- `cargo test --quiet --locked -p uplink-service -p uplink-media --lib --test contracts`: 158 bestanden, 0 fehlgeschlagen, 6 vorhandene Medientests ignoriert. Darin 57 Service-Unit-Tests, 22 Service-Vertragstests und 79 ausgeführte Medien-Unit-Tests.
- `cargo test --quiet --locked -p uplink-media --lib native_2k`: 7 bestanden.
- `cargo clippy --quiet --locked -p uplink-service -p uplink-media --all-targets -- -D warnings`: bestanden. Ein vorhandenes `collapsible_if` im Native-2K-Vertragsprüfer wurde dabei ohne Verhaltensänderung bereinigt.
- Tests mit der vorhandenen Rust-1.97.1-Toolchain; Build-Ausgaben im bestehenden Uplink-Target-Verzeichnis. Keine Änderung am laufenden Dienst oder an anderen Worktrees.

## Nicht ausgeführt / Aktivierungsgrenzen

Keine neue Lastmessung, kein OBS-Stream, kein Twitch-Publish, kein Release-Build, kein Deploy und kein Dienstneustart. Keine Nutzer-ID wurde geraten oder eingetragen; die ausgelieferte Testfreigabe bleibt ungesetzt und `native_2k_av1_units` bleibt null. Das Lesen des Repositorys ist kein Nachweis des aktuell laufenden Release-Stands. Für einen kontrollierten Liveversuch sind geprüfte Aktivierung, die autorisierte numerische Konto-ID, ein freies Wartungsfenster und ein echtes OBS-Signal nötig. Ablauf und Rückkehr stehen in `docs/twitch-native-2k.md`.

Der erste Aufruf des vorhandenen Workspace-Gates mit dem in AGENTS genannten `--review` war nicht ausführbar: das Werkzeug verlangt zusätzlich `--repo` und `--base`. Ein erfolgreicher unabhängiger Gate-Review wird dadurch ausdrücklich nicht behauptet. Ohne diesen Nachweis kein Merge nach main.
