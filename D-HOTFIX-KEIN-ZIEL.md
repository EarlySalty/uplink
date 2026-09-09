# Hotfix: Twitch ohne gemessene Enhanced-Leiter

Stand des Live-Ursachenbeweises: 09.09.2026, 21:51 MESZ. Der erfolgreiche RAM-Statusabruf erfolgte kurz zuvor um etwa 21:50 MESZ; Sekunden wurden dabei nicht separat erfasst. Referenz ist der unveränderte Commit `4b817d8a8c1039a58eb87384626a06175ddc0acd`; Zeilenangaben im Ursachenabschnitt beziehen sich ausschließlich darauf. Neue Test-, Review- und Git-Ergebnisse werden getrennt ergänzt.

## Direkt gemessene Ursache für 538636411

Der jüngste abgeschlossene Live-Status für `streamer_id=538636411` enthält exakt:

```json
{"active":false,"blocked_output":"Der Twitch-Zugang oder sein Kontoinhaber konnte nicht bestätigt werden. Twitch erneut verbinden.","platform":"twitch","streamer_id":538636411}
```

Dieser Text stammt aus `crates/uplink-service/src/media.rs:109`: `publish_grant` scheitert und wird auf den festen sicheren Fehlertext abgebildet. Der Coordinator übergibt ihn in `media.rs:479` an `Reservation::block_output`. Weil kein weiterer Ausgang existiert, gibt `media.rs:482` anschließend „Kein ausführbares Ausgabeziel ist verfügbar; siehe Zielstatus.“ zurück. **Der tatsächlich beobachtete erste Block ist die Brokerprüfung, nicht die erst dahinterliegende Kapazitätsprüfung.**

Sicherer Leseweg: ein temporärer Rust-Adapter aus den bestehenden Beispielen `deployment_metadata.rs` und `ingest_test_probe.rs` verwendete `protect_configured_fds`, `SecretReader` und den bereits autorisierten FD 5 aus `/run/user/1000/credentials/rs-relay.service/infisical-token`. Einziger Dienstaufruf war GET `/v1/me/status?streamer_id=538636411` auf dem bestehenden lokalen API-Listener. Der vorhandene API-Zugang blieb ausschließlich im Rust-Prozessspeicher. Antwortbudget 64 KiB, Gesamtdauer 30 Sekunden, HTTP-Frist fünf Sekunden, keine Proxys oder Weiterleitungen; Ausgabe ausschließlich Nutzer-ID, boolescher Aktivzustand, feste Plattform und gegen statische bekannte Fehlermeldungen geprüfter Sperrgrund. Keine Plattform-/Ingestkeys entschlüsselt, keine Broker-Tokenantwort abgefragt, keine Secrets, Cookies oder Tokenwerte ausgegeben oder in Dateien geschrieben. Der temporäre Adapter wurde nach dem einmaligen Statusabruf entfernt und gehört nicht zum Hotfix.

Der laufende Bot und das Dashboard waren bei der Prüfung bereits auf Release `137fd7834fd3735efaf50cc19d674fafa777854a` (PIDs 3728826 und 3728873), anhand der jeweiligen `/proc/<pid>/exe`-Symlinks lesend bestätigt. Der ältere Betriebsbericht mit `c79f522` ist hierfür nicht mehr aktuell. Beide betreffenden Quellstände haben denselben hier relevanten Brokervertrag:

- `rust/crates/tb-dashboard-api/src/handlers/platform_token.rs:181`: `PlatformTokenQuery` kennt `streamer` und `platform`, kein `purpose`.
- `platform_token.rs:190`: `PlatformTokenAntwort` liefert `connection_generation`, Zugang, Ablauf, Plattform-ID/Login und Scopes, aber weder `purpose` noch `token_owner`.
- `platform_token.rs:388`: auch die Twitch-Erfolgsantwort enthält diese beiden Felder nicht.
- Uplink `crates/uplink-service/src/chat.rs:117` fordert hingegen `purpose=publish`; `chat.rs:239` verlangt `purpose` und `token_owner` als Pflichtfelder. Die Deserialisierung in `chat.rs:247` scheitert bereits bei einer regulären HTTP-200-Antwort des laufenden Brokers. Andere HTTP-/Transportfehler werden ebenfalls abgewiesen. Es wurde keine echte Tokenantwort gelesen; die Vertragsinkompatibilität ist aus den exakt bezeichneten Quellständen bewiesen, der resultierende Sperrtext zusätzlich unmittelbar im Live-Status gemessen.

Die eingebaute Aufforderung „Twitch erneut verbinden“ beseitigt diesen fehlenden Antwortvertrag nicht. Ein Hotfix, der nur die Kapazitätsprüfung überspringt, aber weiter dieselbe neue `publish_grant`-Anforderung vor den gewöhnlichen Zielpfad stellt, würde dieses Konto weiter abweisen.

## Referenz und zwölf rote OBS-Versuche

- `/opt/uplink/current` zeigt auf `/opt/uplink/releases/4b817d8`; `rs-relay.service` ist aktiv/running, MainPID 3441568. `/proc/3441568/exe` zeigt auf dessen `bin/uplink-service`; SHA-256 `45541b4150a51c3baafc9c3e9584a194b9631a0838bcfeb4b443a6e09536fe8e` stimmt mit dem Deploymentbericht überein.
- GET `http://127.0.0.1:8891/v1/health` liefert `ok=true`, `database_ready=true`, `tls_ready=true`, `tls_refresh_failed=false`, `active_sessions=0`, `ingest_test=false`.
- Das Journal enthält im Fenster 21:35–21:50 MESZ genau zwölf passende Abschlüsse desselben Prozesses: 21:40:32 bis 21:41:10 MESZ, Dauer 1306–1384 ms, `source_tracks=2`, `timestamp_clamps=0`, `timestamp_rejection=None`; immer der oben genannte äußere ConsumerClosed-Text.
- Die über lokalen PostgreSQL-Peerzugang (`sudo -n -u postgres psql -X -w`) in `BEGIN READ ONLY` abgefragte Datenbank `deadlock` enthält die zwölf passenden `relay.sessions`-Datensätze 41–52. Startzeiten 19:40:30.695329 bis 19:41:09.492403 UTC, Endzeiten passend zum Journal. Jede Messung bestätigt AV1, 2560×1440, 60/1 fps, YUV420p, BT.709/tv sowie eine AAC-Spur, 48 kHz, Stereo. OBS-Bitrate/AMF-Regelmodus werden aus der Nutzermeldung übernommen; `SourceObservation.rate_control` ist `null` und belegt keine eigene Rate-Control-Messung.
- Jede dieser zwölf Zeilen hat `media_diagnostic=null`; kein gespeichertes `blocked_outputs`-Feld. Das folgt auch aus `registry.rs:373`, wo dieses Feld nicht in den Abschluss aufgenommen wird. Der neu gelesene jüngste RAM-Zielstatus ist der direkte Sperrtextbeweis; die früheren elf Texte wurden nicht nachträglich als separat beobachtet ausgegeben.
- Die einzige gespeicherte aktive Ausgabe ist Twitch: 1920×1080, 60 fps, 6000 kbit/s, Audiowahl `live`, `hochkant_enabled=false`, Fencegeneration 2, `deleted=false`. Es wurden ausschließlich diese sicheren Spalten gelesen; weder Ziel-URL noch verschlüsselter Key oder Zugang wurden ausgewählt.

## Reihenfolge und zusätzlich sichere Kapazitätssperre

Die normale Live-TOML wurde ausschließlich auf nicht geheime Strukturmetadaten projiziert: `[media.enhanced]` fehlt, effektive Kapazität 0, keine Profile; `[chat]` existiert, keine Loopback-Test-CA, öffentlicher Eingang `0.0.0.0:1935`, Twitch-Ausgabecodec H.264.

Auf `4b817d8` geht jedes Produktionstwitchziel in `media.rs:434` am ausschließlich lokalen Testsonderfall vorbei und ab `media.rs:441` durch `twitch_output`. Die Reihenfolge lautet Broker (`media.rs:102`), gespeicherte positive Generation (`media.rs:112`, Prüfung `media.rs:326`), optionale Hochkantwahl, Preferences, Hardware-OnceCell (`media.rs:155`), GoLive (`media.rs:162`), anschließend gemessenes Profil/Kapazität (`media.rs:447`).

Die aktuell gespeicherte Generation ist 2: eine pauschale Diagnose `COALESCE(generation,0)=0` ist für dieses Konto nicht belegt. Der tatsächlich beobachtete Brokerfehler verhindert schon die Generationsprüfung. Hardwaremessung, ihr über die OnceCell gespeichertes Ergebnis und GoLive werden in diesem Pfad ebenfalls nicht erreicht; es gibt keinen Beleg eines erfolgreichen GoLive-Aufrufs dieser zwölf Sessions.

Unabhängig davon ist die folgende zweite Sperre aus Config und Code zwingend: `EnhancedConfig::default` in `config.rs:316` setzt `capacity_units=0` und `profiles=[]`. Ohne passenden Profilkey erzeugt `media.rs:458` den Fehler „Dieses Qualitätsprofil ist noch nicht durch eine Lastmessung freigegeben.“; `registry.rs:198` verweigert denselben Text bei Einheiten 0 oder Kapazitätslimit 0. Somit könnte auf diesem Stand selbst eine erfolgreiche Broker-/GoLive-Integration keinen Twitch-Ausgang starten. Im tatsächlich gemessenen Lauf wurde diese Stelle noch nicht erreicht: sie hätte `media_diagnostic.phase=capacity` gesetzt (`media.rs:460`), das `registry.rs:354` außerdem an den Abschlussgrund anhängt; beides fehlt in allen zwölf Abschlüssen.

`D-LAST.md` in `/home/nathanael/Documents/.tasks/2026-09-09-uplink-enhanced-broadcasting/` bestätigt fehlende Lastmessung: die Baseline endete negativ, kein synthetischer Smoke und keine Kapazitätsfreigabe. Es wird keine gemessene Leiter oder Parallelkapazität erfunden. Vereinbarter Eingang bleibt OBS-AV1/HQCBR 10 Mbit/s, 2560×1440@60 mit einer AAC-Spur; ein Wechsel des Nutzers auf H.264 ist kein Fix.

## Rot/Grün und Review-Halt

Live-Referenz **ROT**: zwölf reproduzierte Abbrüche, konkreter Broker-Sperrgrund unmittelbar im Status bestätigt; zusätzliche zwingende Profil-/Kapazitätssperre nachgewiesen. Health-grün ist kein Ausgabebeweis.

Lokaler Vorher-Test **ROT**: Die neu hinzugefügte Einzelzielregression brach vor dem Fix nach 366 ms mit `ConsumerClosed: Kein ausführbares Ausgabeziel ist verfügbar; siehe Zielstatus.` ab (Cargo Exit 101; anfängliche synthetische H.264-/Ein-AAC-Quelle). Der Builder hat den Rotlauf im Tooltranskript belegt, dafür wurde keine eigene Logdatei gespeichert.

Finaler Hotfix **lokal GRÜN**. Alle Cargo-Aufrufe liefen über `/home/nathanael/.cargo/bin/cargo`, Tests mit `-j 4`:

| Prüfung | Ergebnis |
| --- | --- |
| `cargo test --locked -j 4 -p uplink-service --lib` | 43/43 bestanden; einschließlich Kapazität null, fehlendem Profilkey, positiver Zulassung sowie Journalentprellung über zwölf Reconnects, Grundwechsel, Zeitablauf und 1024-Eintragsgrenze |
| `cargo test --locked -j 4 -p uplink-service --test normalbetrieb -- --ignored` | 11/11 bestanden; Log `/tmp/uplink-hotfix-normalbetrieb.log` |
| `cargo test --workspace --locked -j 4` | vollständiger Neulauf Exit 0, 424 bestanden, 64 absichtlich ignorierte Integrationstests; Log `/tmp/uplink-hotfix-workspace-tests.log` |
| `cargo fmt --all --check` | Exit 0 |
| `cargo clippy --workspace --all-targets --locked -j 4 -- -D warnings` | Exit 0; Log `/tmp/uplink-hotfix-clippy.log` |
| `cargo check --workspace --locked -j 4` | Exit 0; Log `/tmp/uplink-hotfix-check.log` |

Der erste Workspace-Lauf hatte einen Timeout in `cancelled_start_drops_unresponsive_peer_transport` (`pusher.rs:473`), während die übrigen 22 Pusher-Tests bestanden. Dieser Pfad wurde durch den Hotfix nicht geändert. Der vollständige Wiederholungslauf war grün; der erste Rotlauf wird damit nicht als nie aufgetreten dargestellt.

`twitch_without_enhanced_profiles_sends_desired_output_and_stays_alive` prüft den echten Coordinator-Produktionstwitchzweig: Eine zweite synthetische Hostfreigabe macht `lokales_testziel` ausdrücklich falsch. Das gespeicherte, tenantgebunden verschlüsselte Ziel sendet ohne Enhanced-Profile/Kapazität sowie ohne Brokeraufruf eine einzelne x264-Ausgabe. Quelle ist synthetisches AV1/BT.709 mit einer AAC-Spur, Ausgabe enthält 50 H.264-Videoframes und bytegenau erhaltene AAC-Pakete. Zwei weitere Sekunden nach dem Sending-Status bleibt die Session aktiv und fehlerfrei; die nicht gemessene Leiter steht sichtbar im Zielstatus. Die anfängliche AV1-FLV-Fixture meldete GBR-Farbmetadaten, deshalb erzeugt der finale Test sein passendes Material mit dem gepinnten FFmpeg und libsvtav1; keine Produktionsfarbregel wurde abgeschwächt.

`production_twitch_with_profile_zero_capacity_and_no_generation_stays_blocked` hält bei gesetztem Profil den strengen Pfad rot. Er endet bereits an der fehlenden Generation; die nachgelagerte Kapazität null und der fehlende Profilkey werden jeweils gesondert auf der tatsächlich verwendeten Zulassungsfunktion negativ geprüft. Die bisherigen Loopback-Normalfälle bleiben grün.

Finaler SHA-256 des Diffs der vier Rustdateien: `2074195e5241f53088e4915008310d520ac1eb0776875f5c43fd2c74c778ff12`. Testbinary `normalbetrieb-bb3c436269f4215e`, SHA-256 `dfab2896e6ca1438be6511b816bdc9f7400c32580d35ea57871adecc039d97b1`. Interne unabhängige Rust- und Sicherheitsprüfung melden keine BLOCKING-Befunde; die jeweiligen finalen Diffnachprüfungen stehen in [D-HOTFIX-RUST-REVIEW.md](D-HOTFIX-RUST-REVIEW.md) und [D-HOTFIX-SECURITY-REVIEW.md](D-HOTFIX-SECURITY-REVIEW.md).

Grenze: Die Regression verwendet 320×180@25, keine 2560×1440@60-Lastmessung. Sie beweist weder eine gemessene Leiter noch tatsächliche Twitch-Annahme oder den erneuten OBS-Livebeweis. Noch keine grüne neue Produktionsausgabe behauptet. Keine Konfiguration, Datenbankzeile, Plattformverbindung oder Dienste verändert; kein Produktionsstream begonnen oder Dienst neu gestartet.

## Eigen-Gate und Gitnachweis

Der Arbeitsbranch basiert exakt auf `4b817d8`. Der vorhandene geteilte lokale `main` bleibt ausdrücklich unverändert auf `77eae8d`. Für den vorgeschriebenen bestehenden Gate wurde deshalb ohne Netzwerk ein eigener lokaler Clone erstellt: `git clone --shared --no-checkout /home/nathanael/.worktrees/uplink-hotfix-kein-ziel /home/nathanael/.worktrees/uplink-hotfix-kein-ziel-gate`. Dessen origin ist ausschließlich der lokale Hotfixworktree; sein eigener `main` zeigt auf `4b817d8a8c1039a58eb87384626a06175ddc0acd`.

Nach dem Hotfixcommit wird dessen exakter SHA dort detached ausgecheckt und die Identität von SHA und Tree mit dem Arbeitsworktree geprüft. Der bestehende Gate wird von der Hauptsession mit dem vollständigen eingecheckten Ergebnis ausgeführt:

```sh
/home/nathanael/.venvs/gpt-workers/bin/python \
  /home/nathanael/Documents/.claude/gpt-workers/gate_hook.py --review \
  --repo /home/nathanael/.worktrees/uplink-hotfix-kein-ziel-gate --base main --head HEAD
```

Hotfixcommit: `29068d458439fda9df9f3b1f18ec85a5df3c9c88`, Tree `a195e9c3850f1c4222772243445c64946bbb5986`. Arbeitsworktree und detached Gate-Clone zeigten exakt denselben SHA und Tree; beide waren sauber. Der Gate-Clone hatte als Basis exakt `4b817d8a8c1039a58eb87384626a06175ddc0acd`.

Der obige bestehende Eigen-Gate endete für diesen Commit mit **Exit 0** und folgendem vollständigen Urteil (Log `/tmp/uplink-hotfix-gate.log`):

```text
ALLOW: No blocking defect found in the supplied diff.

Repository tools were unavailable, so I could not independently verify HEAD or inspect surrounding code. No findings.
```

**Aussagegrenze:** Der Gate hat nur den gelieferten Diff geprüft; seine Repositorywerkzeuge waren nicht verfügbar. SHA-/Tree-/Basisidentität wurden deshalb ausdrücklich separat in beiden Checkouts geprüft. Die unabhängigen internen Rust- und Sicherheitsreviews haben zusätzlich den tatsächlichen Code und die erreichbaren unveränderten Grenzen gelesen. Kein BLOCKING wurde gemeldet; die eingeschränkte Gateprüfung ersetzt den geforderten frischen Fremdreview nicht.

Der geprüfte Codecommit `29068d458439fda9df9f3b1f18ec85a5df3c9c88` wurde ausdrücklich und ohne Force auf `origin/fix/twitch-ohne-lastmessung-sendet` gepusht; `git ls-remote --heads` bestätigte genau diesen SHA. [Draft-PR #20](https://github.com/EarlySalty/uplink/pull/20) ist gegen `main` eröffnet. Der PR-Text nennt den eingeschränkten Eigen-Gate und den frischen Fremdreview-Halt. Die Ergänzungen nach dem Codecommit betreffen ausschließlich Nachweisdokumente. Der bestehende Gate wird auf dem finalen Dokumentations-HEAD nochmals ausgeführt, bevor dieser Branchstand fast-forward gepusht wird. Die endgültige Prüf-SHA, der finale Remote-HEAD und die CI-Belege werden im externen Betriebsstatus `/home/nathanael/Documents/.tasks/2026-09-09-uplink-enhanced-broadcasting/D-STATUS.md` festgehalten. Der Gate-Clone wird niemals zu GitHub gepusht; es gibt keinen Force-Push und keinen parallelen Gate.

Letzte rein lesende Liveprüfung: **09.09.2026, 21:59:13 MESZ**. Link weiterhin `/opt/uplink/releases/4b817d8`, `rs-relay.service` active/running, MainPID 3441568. Health `ok=true`, `database_ready=true`, `tls_ready=true`, `tls_refresh_failed=false`, `active_sessions=0`.

Der vorhandene externe `D-REVIEW-FREMD.md` mit ALLOW betrifft ausschließlich `b14f272` gegen `a3c88bd` (frühere PR #19). Er ist **keine frische Hotfixfreigabe**. Merge und Deploy bleiben bis zu einem neuen passenden ALLOW für den Hotfix angehalten, sofern der Nutzer keinen ausdrücklich sofortigen Live-Hotfix anordnet.
