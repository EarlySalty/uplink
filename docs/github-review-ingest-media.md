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
| [PR 5: Wrapperbudget](https://github.com/EarlySalty/uplink/pull/5#discussion_r3960650501) | Behoben in `crates/uplink-media/src/{lib,engine,prepare}.rs`: interne Routinggrenzen reservieren maximal 5 zusätzliche Bytes für den Multitrack-Wrapper. Eingangs-/FFmpeg-Grenzen bleiben unverändert; FLV- und Semaphore-Grenzen werden eingehalten, tatsächlich gepufferte Bytes weiterhin gezählt. | `maximum_sized_video_and_audio_allow_multitrack_wrappers` prüft Audio und Video an der Eingangsgrößengrenze. |
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
