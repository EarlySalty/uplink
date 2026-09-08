# GitHub-Review: Ingest und Medienpfad

Stand: 8. September 2026, Ausgangscommit
`da83eb3e9d3979aad5d5f5cc8c64963005dfb888`.
Umfang: PR 3, PR 5 und der zugewiesene Hilfsaudio-Hinweis aus PR 4.
Neun weiterhin vorhandene Fehler sind in diesem Paket korrigiert. Die
AV1-CTS-Behauptung ist fachlich falsch; der Parser bleibt unverändert.

## Befunde und Zuordnung

| Kommentar | Ergebnis und betroffene Dateien | Nachweis |
| --- | --- | --- |
| [PR 3: AV1 CTS](https://github.com/EarlySalty/uplink/pull/3#discussion_r3958356775) | Begründet falsch. `crates/uplink-ingest/src/media.rs` behandelt AV1 bereits ohne SI24; explizit dokumentiert seit `63b11e6`. Nur Regression erweitert. | `multivideo_av1_preserves_payload_without_inventing_cts` prüft jetzt Singletrack und Multitrack. |
| [PR 3: kleine Chunks](https://github.com/EarlySalty/uplink/pull/3#discussion_r3958356780) | Behoben in `third_party/scuffle-rtmp/src/chunk/reader.rs`: verhandelte Größen ab 1 Byte erlaubt, Obergrenze bleibt bestehen. | `negotiated_small_chunks_reassemble_a_message`: echte Reassemblierung für 1, 7, 64 und 127 Byte; 0 und Übergröße abgewiesen. |
| [PR 3: MetadataKeepalive](https://github.com/EarlySalty/uplink/pull/3#discussion_r3958356784) | Behoben ausschließlich in `Handler::media`, `crates/uplink-ingest/src/server.rs`: nur Header und Frames erneuern Medienaktivität. | TLS-Integration `metadata_does_not_extend_the_missing_media_deadline` in `crates/uplink-ingest/tests/transport.rs`. |
| [PR 3: capsEx](https://github.com/EarlySalty/uplink/pull/3#discussion_r3958356788) | Behoben in `third_party/scuffle-rtmp/src/command_messages/netconnection/mod.rs`: expliziter Wire-Name `capsEx`. | `connect_reads_enhanced_wire_capabilities` in `netconnection/reader.rs` dekodiert echte AMF-Daten und prüft Reconnect/Multitrack. |
| [PR 5: Codecangebot](https://github.com/EarlySalty/uplink/pull/5#discussion_r3960650494) | Behoben in `crates/uplink-media/src/platform/twitch.rs`: Configure bietet nur erfolgreich initialisiertes H.264/libx264 an, das der Encoder-Vertrag tatsächlich übersetzen kann. Die reine Probe darf weiterhin weitere Codecs messen. | `configure_advertises_only_the_proven_encoder_translation`. |
| [PR 5: Wrapperbudget](https://github.com/EarlySalty/uplink/pull/5#discussion_r3960650501) | Behoben in `crates/uplink-media/src/{lib,engine,prepare,flv}.rs`: interne Routinggrenzen reservieren maximal 5 zusätzliche Bytes für den Multitrack-Wrapper. Ursprüngliche Tags und neu codiertes Video behalten ihr Limit. Kopiertes AAC behält die Reserve auch hinter FFmpeg; FLV- und Semaphore-Grenzen sowie reale Queue-Bytes bleiben begrenzt. | `maximum_sized_video_and_audio_allow_multitrack_wrappers` plus Worker-Reader-Regressionen im Nachfix. |
| [PR 5: temporäre Auth-Query](https://github.com/EarlySalty/uplink/pull/5#discussion_r3960650509) | Behoben in `crates/uplink-media/src/platform/twitch.rs`: Query der aktiven temporären Zugangsdaten bleibt neben Endpoint- und Original-Query erhalten. Widersprüche und fremde `clientConfigId` werden abgewiesen. | `temporary_authentication_keeps_its_own_query`, `conflicting_or_reserved_temporary_auth_queries_are_rejected`. |
| [PR 5: Probediagnostik](https://github.com/EarlySalty/uplink/pull/5#discussion_r3960650521) | Behoben in `crates/uplink-platform-probe/src/main.rs` und `crates/uplink-media/src/platform/twitch.rs`: konkrete statische Fehlerursache statt Sammelfehler; keine Servertexte oder Zugangsdaten ausgegeben. | `probe_error_messages_keep_distinct_static_causes`; vorhandene Tests für sichere Fehlertexte bleiben grün. |
| [PR 5: falsche Spur-ID](https://github.com/EarlySalty/uplink/pull/5#discussion_r3960650527) | Behoben in `crates/uplink-media/examples/media_probe/platform.rs`: Zielspur 0 wird ausdrücklich geprüft; falsche ID liefert einen Fehler statt Map-Index-Panik. | `wrong_single_wire_id_returns_mapping_error` für IDs 1, 5 und 9 sowie korrekte ID 0. |
| [PR 4: ungewählte Hilfsaudiospur](https://github.com/EarlySalty/uplink/pull/4#discussion_r3959995383) | Behoben in `crates/uplink-media/src/{graph,engine}.rs`: nur gewählte Audiospuren validieren und einlesen, Session-/Generationsprüfung weiterhin vor dem Überspringen. Gilt für normale und Programmausgänge. | `unused_auxiliary_audio_does_not_block_selected_mix`; bestehender Multivideo-Graph-Test zusätzlich mit ungewählter 44,1-kHz-Spur. Gewählte ungültige Spur wird weiterhin abgewiesen. |

Zusätzlich dokumentiert `third_party/scuffle-rtmp/PATCHES.md` die beiden
Vendor-Korrekturen. Admission-/Permit-Lifecycle und Servicekonfiguration gehören
zum getrennten, anschließend gemeinsam zu prüfenden Paket.

## Primärquellen

Die [Enhanced-RTMP-V2-Spezifikation, ExVideoTagBody](https://veovera.org/docs/enhanced/enhanced-rtmp-v2.html#enhanced-video)
(v2-2026-01-31-r2) enthält für `CodedFrames` getrennte Codec-Zweige:
AV1 trägt unmittelbar `av1CodedData`; SI24-Kompositionsoffsets stehen in den
AVC-/HEVC-/VVC-Zweigen. Drei vermeintliche CTS-Bytes bei AV1 abzuschneiden würde
Nutzdaten zerstören. Dieselbe Spezifikation definiert die Connect-Eigenschaft
`capsEx`.

Die [RTMP-Spezifikation, Abschnitt 5.4.1](https://veovera.org/docs/legacy/rtmp-v1-0-spec.pdf)
unterscheidet die Standardgröße 128 von der erlaubten Mindestgröße 1.

## Ausgeführte Prüfungen

Rust-Toolchain 1.97.1; alle Cargo-Aufrufe mit `--locked --jobs 2` und isoliertem
`--target-dir /home/nathanael/.cache/uplink-ingest-media-review-target`.

- Vendor-Tests (`--manifest-path third_party/scuffle-rtmp/Cargo.toml --lib`):
  **86 bestanden, 2 ignoriert**. `test_basic_rtmp_clean` und
  `test_basic_rtmp_unclean` benötigen externe Upstream-Medienassets/FFmpeg und
  zählen ausdrücklich nicht als getestet.
- Ingest/Media `--all-targets`: Ingest **16 Unit-, 7 HEVC-/Multivideo- und
  19 Transporttests**, Media **41 Unit- und 23 Pusher-Integrationstests** sowie
  **1 Beispieltest** bestanden.
- Nach den zwei ergänzten Query-/Diagnoseregressionen: Media
  `--lib platform::twitch::tests`, **10 bestanden**, 33 weitere nicht erneut
  ausgeführt. Der vorherige volle Lauf bleibt der Nachweis für diese Fälle.
- Clippy für `uplink-ingest`, `uplink-media`, `uplink-platform-probe` mit
  `--all-targets -- -D warnings`: bestanden.
- `cargo build -p uplink-platform-probe`: bestanden.
- Sieben neue Regressionen wurden vor den Ursachenfixes tatsächlich rot
  ausgeführt: kleine Chunks, capsEx, MetadataKeepalive, Codecangebot,
  Wrapperbudget, temporäre Auth-Query und ungewählte Hilfsaudiospur.
- Formatierung nur geänderter Rust-Dateien; `git diff --check` bestanden.

Kein öffentlicher Plattformstream, keine OBS-Abnahme und keine Aussage über
eine Plattformveröffentlichung. Autor-Gate und unabhängiges Rust-Review werden
mit dem eingefrorenen Commit separat protokolliert; dieses Dokument ersetzt
keinen dieser Nachweise.

## Nachfix aus dem unabhängigen Rust-Review

Basis des Nachfixes: `bacd8f217518ed13e6ebafb6c18d100fd682e720`.
Der unabhängige Review hat zwei verbleibende Fehler reproduziert:
`prepare_source` prüfte revidierte ungewählte Audiospuren noch vor der Auswahl;
der Worker-Reader wies intern vergrößertes kopiertes AAC weiter am Eingangslimit
ab. Beide Ursachen sind im Nachfix geschlossen.

- `prepare.rs`: Beide öffentlichen Prepare-Einstiege reichen ihre gewählten
  Audio-Wire-IDs an den Vorlauf. Session/Generation, ursprüngliche Taggröße und
  gesamtes Vorlauf-Byte-/Eventbudget werden vor dem Auslassen geprüft. Aux-Tags
  landen weder in der Revisionsprüfung noch im FFprobe-Eingang oder Prefix.
- `flv.rs` / `engine.rs`: Der tatsächliche Worker-Reader erlaubt höchstens fünf
  zusätzliche Bytes ausschließlich für AAC-OneTrack-Header/-Frames. Legacy-AAC,
  andere Codecs, Metadaten und neue Videotags erhalten keine Reserve.
- `prepare/review_tests.rs`: Die beiden Originalrepros sind in die Suite
  übernommen. Zusätzlich laufen beide öffentlichen Einstiege bis zum lokalen
  TLS-Ausgang: jeweils 50 Videoframes und bytegleiche AAC-Payloads samt Zeitlinie
  trotz zweitem Header auf ungewählter 44,1-kHz-Spur 7. Für den strikten
  Program-Farbvertrag schreibt FFmpeg zuvor echte BT.709-VUI-Daten in die
  synthetische Fixture. Gewählte revidierte Spuren bleiben abgewiesen.
- `ignored_audio_still_obeys_identity_and_prelude_limits` prüft fünf Fälle:
  fremde Session, fremde Generation, Eventbudget, Bytebudget, Originaltaggröße.
  `worker_reserve_only_accepts_bounded_copied_aac` prüft AAC-Grenzfälle und die
  unveränderten Grenzen für andere Tags ohne externen Medienbuild.

Eigener Rotlauf vor dem Ursachenfix: **3 rot, 1 grün** (Exit 101), einschließlich
beider Originalrepros. Echter FFmpeg-Kopierlauf: größter AAC-Eingangskörper
**237 Bytes**, mit Wrapper **242 Bytes**. Danach gezielt
`cargo test -p uplink-media --lib prepare::review_tests -- --include-ignored --nocapture`:
**5 bestanden**, einschließlich beider Originalrepros und beider öffentlicher
Einstiege. Der AAC-Originalrepro verwendet jetzt dieselbe Reader-Konstruktion
wie der Worker; der gewöhnliche Reader behält ausdrücklich das Eingangslimit.

Abschließend `cargo test -p uplink-media --all-targets`: **45 Unit-, 23 Pusher-
und 1 Beispieltest bestanden**. Die vier standardmäßig ignorierten FFmpeg-Fälle
sind im vorgenannten gezielten Lauf ausdrücklich ausgeführt und grün. Sie
benötigen `/opt/uplink/media/ffmpeg8-c733b4b2/{ffmpeg,ffprobe}` und laufen ohne
Produktionszugänge. Clippy für `uplink-media --all-targets -- -D warnings` sowie
`git diff --check` bestanden. Keine erneute unbetroffene Workspace-/Vendor-
Breitenprüfung; keine Aussage über öffentliche Plattformannahme oder OBS.
