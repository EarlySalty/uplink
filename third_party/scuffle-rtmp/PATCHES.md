# Lokaler RTMP-Patch

Unveröffentlichte lokale Ableitung von `scuffle-rtmp 0.2.3`, kein Code aus
rs-relay und keine neue vollständige RTMP-Implementierung.

- Quelle: https://crates.io/crates/scuffle-rtmp/0.2.3
- Originalarchiv-SHA256: `8c2da6c55454996d5654c7f56fcf5db9c9b48a82ece219b22badebdb9782de0a`
- Upstream-Git: `2445114cc326ca8862a72355db555fd0a7fa16f9`, Verzeichnis `crates/rtmp`
- Lizenz: `MIT OR Apache-2.0`; beide Lizenzdateien unverändert übernommen.
- `Cargo.toml.orig` bewahrt das ursprüngliche Manifest. Das lokale Manifest
  enthält einen eigenen Workspace, pinnt den benachbarten AMF-Patch auf 0.2.4,
  entfernt den direkten Workspace-Hack und das nicht mitgelieferte Example.
  Der nur in Upstream-Tests verwendete FutureExt liegt in Dev-Dependencies.
  Über andere Scuffle-Crates kann Workspace-Hack transitiv im Lockfile bleiben.

Die Änderung vom 8. September 2026 betrifft Ressourcenbudgets vor Chunk-/AMF-
Allokationen, gültige Zeitstempelarithmetik, ACK-Zählung, Sitzungsfristen,
EOF, TLS-Flush, Publish-Grenzen und die Redaktion fremder Daten in Logs/Fehlern.
Die konkrete API, Grenzen, Tests und Reproduktion stehen in
[docs/third-party-patches.md](../../docs/third-party-patches.md).

Semantisch geänderte Quelldateien: `chunk/reader.rs`, `chunk/error.rs`,
`command_messages/reader.rs`, `command_messages/error.rs`, `messages/reader.rs`,
`error.rs`, `protocol_control_messages/reader.rs`, `session/server/mod.rs`, `session/server/error.rs`,
`session/server/handler.rs`. Neu sind `session/server/limits.rs`,
`session/server/output.rs` sowie die beiden `uplink_tests.rs`.
`lib.rs` markiert zwei nicht portable Upstream-Tests ausdrücklich als ignoriert.
Andere Quelldateien tragen nur Format-/Herkunftsänderungen.

Nachkorrektur vor Push: Die Parser für SetChunkSize und WindowAcknowledgementSize
verlangen einen vollständigen Body mit genau vier Byte. Andere Längen ergeben
`InvalidData` statt eines aus Cursor-Lesen entstandenen `UnexpectedEof`.
Ein fehlerhafter Nachrichtenkörper wird dadurch nicht länger als geschlossener
Transport gemeldet. Beide offenen-Transport-Fälle wurden zuerst in Debug und
Release rot gemessen; echte EOF- und Timeout-Semantik bleiben erhalten.

Nachkorrektur des ACK-Fensterwechsels: Nach einer gültigen Änderung wird
dieselbe Fälligkeitsprüfung wie nach einem Read angewendet. Bereits empfangene,
noch nicht bestätigte Bytes können durch ein kleineres Fenster sofort fällig
werden; das ACK wird im vorhandenen Nachrichten-Flush vor dem nächsten Read
gesendet. Keine zusätzliche Schleife oder Bestätigung bereits quittierter Bytes.
Eine vollständige Session mit wartendem Peer scheiterte zuvor in Debug/Release
und besteht nach dem Fix; Fenstervergrößerung, Null-Abweisung, mehrfacher Wechsel
und Wire-Zähler-Wrap sind ebenfalls geprüft.

GitHub-Nachreview vom 8. September 2026: `chunk/reader.rs` akzeptiert
ausgehandelte Chunkgrößen von 1 bis zur konfigurierten Obergrenze; 128 bleibt
der Startwert, entsprechend RTMP 5.4.1. `command_messages/netconnection/mod.rs`
ordnet das E-RTMP-Drahtfeld `capsEx` explizit dem typisierten Feld zu.
Die Regressionen lesen echte fragmentierte Chunks beziehungsweise eine
AMF-kodierte Connect-Nachricht. Damit enthält auch
`command_messages/netconnection/reader.rs` einen ergänzten semantischen Test.

Die Bibliothek allein ist weder eine öffentlich freigegebene Ingest-Schnittstelle
noch eine Codec-, OBS-, Twitch- oder Plattformfreigabe. Die aufrufende Anwendung
verantwortet TLS, Autorisierung, Sessionanzahl, Medienkopien und Eventbudgets.
EOF und Zeitüberschreitung bleiben unterscheidbar. AMF-Fehlerquellen können
intern Daten enthalten; nur redigierte Fehlerdarstellungen weitergeben.
