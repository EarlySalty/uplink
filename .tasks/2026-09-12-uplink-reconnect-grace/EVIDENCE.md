# Uplink Reconnect Grace – Nachweis

Stand: 12. September 2026. Vorbereitung ausschließlich im Worktree `uplink-reconnect-grace-20260912`; kein Deploy und kein Dienstrestart.

## Ursache

Der normale Medienpfad behandelte jedes Ende des OBS-Eingangskanals wie einen sauberen Streamabschluss. Dadurch rief der Plattform-Pusher `finish()` auf und sendete RTMP `deleteStream` auch dann, wenn die Quelle wegen `PeerClosed`, `MediaTimeout` oder `ProtocolTimeout` unerwartet abgerissen war. Twitch bekam damit ein ausdrückliches Ende statt eines reconnect-fähigen Transportabbruchs.

## Änderung

- Der endgültige Ingest-`EndReason` wird vor dem Schließen des Medienkanals an die Media-Engine übergeben.
- Nur transportartige Unterbrechungen (`PeerClosed`, `MediaTimeout`, `ProtocolTimeout`, `TaskFailed`) werden als `SourceTermination::Interrupted` behandelt. Expliziter Stop und kontrollierte Medien-/Serverfehler bleiben normale Abschlüsse.
- Bei `Interrupted` schließt `RunningPusher::interrupt()` die RTMP(S)-Verbindung ohne `deleteStream`. Der nächste OBS-Reconnect erzeugt dadurch keinen von Uplink aktiv signalisierten Stream-Ende-Befehl.
- Der Ausgangsstatus unterscheidet `interrupted` von `local_end_unconfirmed` und `failed`.
- Journaltexte unterscheiden nun `ExplicitStop`, `PeerClosed` und `MediaTimeout`; `PeerClosed` wird nicht mehr als „Streamer hat beendet“ bezeichnet.

## Diagnose

Der Ingest erfasst ausschließlich Timing-Metadaten, keine Medieninhalte:

- auffällige globale Medienlücken ab 500 ms,
- begrenzter Tail der letzten 32 Lücken,
- Gesamtzahl und maximale Lückendauer,
- Recovery-Zeit relativ zum Sessionstart sowie Track/Event-Typ,
- persistiert in `relay.sessions.profile_json`,
- Recovery zusätzlich als sichere Journalzeile.

## Tests

Alle Befehle wurden mit Cargo/Rustc 1.97.1 ausgeführt.

- `cargo test -p uplink-ingest -p uplink-media -p uplink-service --lib --locked -j2`: 18 + 72 + 52 bestanden, 6 bestehende FFmpeg-Tests ignoriert.
- `cargo test -p uplink-media --test pusher interrupted_source_closes_transport_without_explicit_unpublish --locked -j2 -- --nocapture`: bestanden; Zielseite meldet `PeerClosed`, nicht `ExplicitStop`.
- `cargo test -p uplink-service --tests --locked -j2`: alle nicht ignorierten Service-Tests bestanden.
- `cargo clippy -p uplink-ingest -p uplink-media -p uplink-service --all-targets --locked -j2 -- -D warnings`: bestanden.

## Betrieb

Keine produktive Datei, kein `/opt/uplink/current`-Symlink und kein systemd-Dienst wurde durch diese Arbeit verändert. Deployment ist ausdrücklich auf einen Zeitpunkt nach Ende des laufenden Streams verschoben.
