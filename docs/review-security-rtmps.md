# Sicherheitsreview: lokaler RTMPS-Eingang

Stand: 8. September 2026. Unabhängige Prüfung des Arbeitsstands auf
`feat/rtmps-ingest` gegen `02567c2d865825e374c7c5ead78e84f200307608`, einschließlich
des korrigierten Metadata-Tracklimits und der Codec-/Headerzuordnung.
Ergänzend wurde der enge Gate-Nachfix gegen
`baf240cf53b6460d20cb276e96c20dadd45a6f4a` unabhängig geprüft: Abbruch von
`finish()` und Messung des beobachteten Eventbudgets.
Ein weiterer enger Nachreview gegen
`b6c18f55fd30b6fc0669e444de5eceeae2f99751` prüft die Trennung beschädigter
Kontrollnachrichten von einem tatsächlichen Transport-EOF.

**Ergebnis:** Kein weiterer konkreter Sicherheitsblocker im geprüften
Loopback-Baustein. Der Stand kann für diesen Entwicklungsumfang übernommen
werden. Dies ist keine Freigabe für öffentlichen Ingest, produktive
Kontoverbindungen, OBS oder Plattformausgänge.

## Prüfbereich und Quellstand

Geprüft wurden `crates/uplink-ingest`, die tatsächlich verwendeten
Parser-/Serverpfade in `third_party/scuffle-rtmp` und `third_party/scuffle-amf`,
deren Manifeste/Lockfiles sowie Root-Manifest, Root-Lockfile und Rust-CI.
Die fachliche Protokoll-/Medienprüfung erfolgt zusätzlich im gekoppelten
[Rust-Review](review-rtmps.md). Andere Dienste und vorhandene Zugangsdaten wurden
nicht untersucht oder geändert.

SHA-256 über die sortierten Pfade und SHA-256-Werte aller Rust-Dateien und
Manifeste/Lockfiles dieses Bereichs einschließlich Root-Cargo/CI:

`40d22f5d4f51c06b467354f498c6635bda6c90dcd369b718f457d2eeacb6235b`

Reproduktion aus der Repositorywurzel; Buildverzeichnisse bleiben ausgeschlossen:

```sh
rg --files crates/uplink-ingest third_party/scuffle-rtmp third_party/scuffle-amf \
  -g '*.rs' -g Cargo.toml -g Cargo.lock -g '!**/target/**' \
  | awk 'BEGIN { print "Cargo.toml"; print "Cargo.lock"; print ".github/workflows/rust.yml" } { print }' \
  | sort | xargs sha256sum | sha256sum
```

## Wesentliche Befunde

- **Zugang:** `IngestServer::bind_loopback` bindet fest an `127.0.0.1`; die
  Peer-Adresse wird zusätzlich geprüft (`server.rs:202`, `server.rs:236`). Ein
  Slot wird vor TLS-Arbeit und Taskstart reserviert. Der Publish-Callback muss
  eine gültige, nicht leere Nutzer-/Sessionidentität liefern. Sowohl der
  RTMP-Server als auch der Ingest-Handler prüfen die autorisierte Stream-ID vor
  Medienübergabe. Ein weiterer Publish derselben Verbindung wird abgelehnt.
  Frische Instanzkennung und nicht überlaufender Verbindungszähler trennen
  Generationen. Der vorhandene produktive Broker ist ausdrücklich noch nicht
  angeschlossen; die feste Identität im Beispiel ist synthetisch.
- **Ressourcen vor Allokation:** Chunknachrichten werden nach ihrem Header vor
  Payloadübernahme gegen Byte-, CSID-, Partialanzahl- und Partialgesamtbudget
  geprüft (`chunk/reader.rs:204`). Kontrollierte Read-/Write-Puffer begrenzen
  Transportansammlungen. Native AMF- und Serde-Pfade verwenden dieselben
  Byte-, String-, Eintrags-, Werte- und Containertiefengrenzen. Strikte
  Arraylängen werden vor Größenhinweisen und Reservierungen validiert;
  ECMA-Hinweise lösen keine Vorreservierung aus. Fehlende Terminatoren,
  ausbleibender Serde-Fortschritt und unvollständiger Containerverbrauch führen
  zu Fehlern. Die Eingangs-App verwendet begrenzte Nachrichten statt eines
  unendlich fortgesetzten AMF-Decoders.
- **Fristen und Queue:** Eine gemeinsame absolute Startfrist umfasst TLS,
  RTMP und Autorisierung. Zusätzlich gelten begrenzte Read-, Write-/Flush-
  und Callbackfristen; Kontrollnachrichten verlängern die Medien-Idlefrist
  nicht. Regelmäßiges Yield macht Fristen auch bei bereits gepufferten
  Nachrichten beobachtbar. Medien werden erst nach Byte-/Eventreservierung
  kompakt kopiert (`server.rs:428`). Gehaltene Events behalten diese Budgets
  sowie den Session-Slot bis zu ihrem Drop. Der Metadata-Fix verbraucht jetzt
  ebenfalls das Trackbudget; ein Metadata-Paket erbt keine fremde Codec-Revision.
- **Secretredaktion:** `MediaEvent` besitzt keine Debugdarstellung der
  Nutzdaten. Berichte enthalten Zustände und Zähler, keine App-/Publishnamen,
  Zugangswerte oder Payloads. `CommandError` redigiert AMF-Inhalte in Display
  und Debug; der Ingest wandelt Parserfehler in feste Zustände um. Standardhooks
  protokollieren keine fremden Befehlswerte. Rohe Fehlerquellen können intern
  weiter AMF-Inhalte tragen und dürfen von zukünftigen Aufrufern nicht
  ungefiltert protokolliert werden. Im jetzigen Ingest existiert dieser
  Ausgabepfad nicht.
- **TLS-Probe und Fremdprozess:** Der private Testschlüssel verbleibt im
  Prozessspeicher. Nur das öffentliche, neu generierte Zertifikat wird unter
  zufälligem Namen mit `create_new` temporär angelegt und anschließend entfernt.
  FFmpeg erhält strukturierte Argumente, feste Fixtures und eine feste
  lokale Testadresse; keine Shell und keine echten Zugangsdaten. Ausgaben
  sind auf 64 KiB begrenzt, Versuche und Versionsabfrage haben Fristen.
  Erfolgs- und Fehlerpfade sammeln den Kindprozess ein; Drop beendet ihn
  zusätzlich. Die gemessene FFmpeg-Lücke bei numerischen URL-Hosts wird durch
  den ausdrücklich dokumentierten `localhost`-Testweg ausgeschlossen, nicht
  als bewiesene allgemeine FFmpeg-Hostnameprüfung dargestellt.
- **Abhängigkeiten und CI:** Beide lokalen Ableitungen haben Herkunft,
  Originalarchiv-Prüfsumme und Lizenznachweise. Root und RTMP verwenden den
  lokalen AMF-Patch. Die CI hat nur `contents: read`, keine Secretübergabe,
  keine Deploymentrechte und kein `pull_request_target`. Prüfungen nutzen
  Lockfiles. Kein neuer ENV-Config- oder Secretdateipfad wurde übernommen.

## Eigenständig ausgeführte Prüfungen

Die folgenden Läufe gehören zum ersten vollständigen Review vor dem engen
Gate-Nachfix; dessen zusätzliche Prüfungen stehen unmittelbar darunter.

| Prüfung | Ergebnis |
| --- | --- |
| `cargo +1.97.1 test -p uplink-ingest --test transport --locked -j2` | 15 bestanden, 0 fehlgeschlagen, einschließlich Metadata-Fix, TLS, Autorisierung, Fristen und gehaltenen Eventbudgets |
| `cargo +1.97.1 test --manifest-path third_party/scuffle-rtmp/Cargo.toml --locked --target-dir target -j2 uplink_` | 23 bestandene gezielte Parser-/Serverregressionen |
| `cargo +1.97.1 test --manifest-path third_party/scuffle-amf/Cargo.toml --locked --target-dir target --features serde --test decode_limits -j2` | 21 bestandene native/Serde-Grenztests |
| `cargo audit --file Cargo.lock` | Exit 0, 131 Abhängigkeiten |
| `cargo audit --file third_party/scuffle-rtmp/Cargo.lock` | Exit 0, 97 Abhängigkeiten |
| `cargo audit --file third_party/scuffle-amf/Cargo.lock` | Exit 0, 31 Abhängigkeiten |
| Redigierte Gitleaks-Verzeichnisscans nur über `crates/uplink-ingest` und `third_party` | Exit 0, keine Treffer |
| Gezielte Dateinamenprüfung ohne Buildverzeichnisse | Keine ENV-/PEM-/Key-/P12-/PFX-Dateien im neuen Quellbereich |
| `git diff --check` | Exit 0 |

Die drei Advisory-Prüfungen aktualisierten die RustSec-Datenbank und luden
jeweils 1242 Meldungen. Das beweist keine allgemeine Fehlerfreiheit der
unveröffentlichten Parserpatches. Die Testaufrufe erfolgten über den expliziten
Rustup-Pfad `/home/nathanael/.cargo/bin/cargo`; das im Tool-PATH zuerst gefundene
ältere Cargo konnte Resolver 3 nicht verarbeiten.

Die echte FFmpeg-8-Positivprobe und CA-/SAN-Negativprobe wurden vom zuständigen
Autor nach dem Metadata-Fix erneut ausgeführt und im
[RTMPS-Nachweis](rtmps-nachweis.md) dokumentiert. Sie wurden in diesem
Sicherheitsreview nicht erneut gestartet. Breite Debug-/Releaseprüfungen und
der abschließende Gate-Lauf werden zentral gebündelt.

## Gezielter Nachreview nach dem Gate

Gegen `baf240c` änderten sich ausschließlich `crates/uplink-ingest/src/server.rs`,
der zugehörige Transporttest und `docs/rtmps-nachweis.md`. Die bestehenden
Parser-, Abhängigkeits- und Secretprüfungen wurden deshalb nicht wiederholt.

`finish()` behält den JoinHandle bis nach dem vollständigen Await in `self.task`.
Wird die Future abgebrochen, erreicht `RunningConnection::drop` weiterhin den
Handle und bricht den Producer ab. Nach dessen Ende werden Socket und Slot
freigegeben; extern gehaltene Events behalten ihren bisherigen Slotbesitz.
Zwischen abgeschlossenem Await und Entnahme des Handles liegt keine weitere
Unterbrechungsstelle.

Die Budgetbelegung wird jetzt vor `try_send` erfasst, während der Producer die
Byte-Permits des aktuellen Events hält. Der Bericht verwendet diesen erfassten
Wert auch dann, wenn der Consumer das Event sofort freigibt. Die Änderung
beeinflusst keine Admission- oder Speichergrenze. `max_queued_bytes` ist
ausdrücklich das Maximum dieser Messpunkte: Nebenläufige Drops älterer Events
können Zwischenpeaks verdecken. Rustdoc und Nachweis versprechen deshalb keinen
atomaren historischen Höchststand oder gemessenen RAM-Verbrauch.

Beide neuen Regressionen wurden im Nachreview mit Rust 1.97.1, `--locked -j2`
einzeln ausgeführt und bestanden:

- `--lib immediate_consumer_drop_cannot_erase_observed_event_budget` erzwingt
  das relevante Interleaving über den Berichts-Mutex und prüft vier beobachtete
  Bytes nach vollständiger Consumer-Freigabe.
- `--test transport cancelled_finish_closes_silent_producer_and_releases_connection_slot`
  bricht `finish()` bei einem stillen TCP-Peer ab und prüft Socket-Ende sowie
  eine neue Slotreservierung deutlich vor der konfigurierten 60-Sekunden-Frist.

`git diff --check` blieb erfolgreich. Der oben angegebene Fingerprint wurde
über den aktualisierten Gesamtprüfbereich neu berechnet. Ergebnis des engen
Nachreviews: beide Befunde geschlossen, keine zusätzlichen Sicherheitsblocker
im freigegebenen lokalen Entwicklungsumfang.

## Gezielter Nachreview der EOF-Klassifikation

Der Produktionsdiff gegen `b6c18f5` beschränkt sich auf
`third_party/scuffle-rtmp/src/protocol_control_messages/reader.rs`; zusätzlich
wurden die Serverregressionen in `session/server/uplink_tests.rs` erweitert.
Manifeste, Abhängigkeiten, AMF-Parser und Secretpfade blieben unverändert und
wurden deshalb nicht erneut auditiert oder gescannt.

Beide Kontrollnachrichten-Reader erhalten bereits vollständig gerahmte Bodies.
Sie verlangen nun vor dem Auslesen exakt vier Bytes durch die Konvertierung
in `[u8; 4]` mit `try_into` und wandeln eine abweichende Länge in `InvalidData` um.
Der Fehlertext ist konstant und enthält weder die Eingabebytes noch Zugänge.
Es gibt keine unkontrollierte Allokation und keinen Stringvergleich zur
Fehlerklassifikation. Gültige Werte werden weiterhin Big-Endian gelesen.

Damit bleiben Parserfehler über `RtmpError::Io(InvalidData)` von
`is_client_closed()` ausgeschlossen und erreichen im Ingest den Zustand
`ProtocolRejected`. Die Behandlung eines echten Transport-EOF wurde nicht
geändert. Neben verkürzten Bodies werden auch überlange Bodies abgelehnt.

Vier gezielte Regressionstests wurden eigenständig mit Rust 1.97.1,
`--locked --target-dir target -j2` ausgeführt und bestanden:

- `control_bodies_require_exactly_four_bytes`: beide Reader lehnen die Längen
  0–3 und 5–8 mit `InvalidData` ab.
- `uplink_short_set_chunk_size_is_protocol_error_while_transport_open` und
  `uplink_short_ack_window_is_protocol_error_while_transport_open`: die volle
  Server-Session liefert bei offenem Peer einen Protokollfehler.
- `uplink_valid_control_then_transport_eof_remains_peer_close`: gültige
  Vier-Byte-Nachrichten behalten bei anschließendem tatsächlichem EOF den
  bisherigen normalen Verbindungsabschluss.

`git diff --check` bestand; der Gesamtfingerprint oben wurde aktualisiert.
Ergebnis: Befund geschlossen, keine zusätzlichen Sicherheitsblocker für den
unverändert lokalen Entwicklungsumfang.

## Aussagegrenzen

Die Parserprüfung betrifft die oben beschriebene aufrufende Ingest-Kette mit
ihren Limits. Sie ist weder eine Fuzzkampagne noch eine pauschale Freigabe
aller öffentlich aufrufbaren Hilfsfunktionen der Vendor-Crates. TLS-/Allocator-
Overhead und Spitzen-RSS wurden nicht als Kapazitätsnachweis gemessen.
Öffentlicher Betrieb, echte Secretbeschaffung, Autorisierungswiderruf,
Brokerintegration und Plattformberechtigungen benötigen ihren eigenen
Implementierungs- und Abnahmenachweis.
