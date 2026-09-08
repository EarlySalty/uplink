# Contract: YouTube-Live-Adapter für Uplink

status: aktiv
datum: 2026-09-08
klasse: hoch
repo: uplink

Dieser Contract ist der Maßstab für Implementierung und Merge-Kritiker. Nach dem
Anlegen ist er unveränderlich: der Hook lässt nur noch die `status:`-Zeile und
Anhänge unter `## Amendments` zu. Wer ein REQ oder INV ändern will, schreibt ein
Amendment mit Begründung; Produkt-, API- oder Datenänderungen entscheidet der User.

Auftrag: `/home/nathanael/Documents/.tasks/2026-09-08-uplink-neubau/CLAUDE-YOUTUBE-LIVE.md`.
Schnittstellenvertrag: `docs/youtube-live-vertrag.md`.

## Ziel

Ein Nutzer, der YouTube im Uplink-Dashboard bewusst für Live freigegeben hat, erreicht beim OBS-Start einen korrekt gebundenen YouTube-Broadcast, sieht dessen Zustand und kann ihn beenden; ein weiterer Stream nach abgeschlossenem Broadcast funktioniert ebenso.

## Anforderungen (user-sichtbares Verhalten)

- REQ-01: `prepare` legt ohne gespeicherte Freigabe (`live_freigegeben_at`) nichts bei YouTube an und antwortet `Blockiert(KeineFreigabe)`; eine Freigabe unter älterer Verbindungsgeneration gilt nicht.
- REQ-02: `prepare` prüft einen gespeicherten Stream per `liveStreams.list`, legt sonst einen neuen wiederverwendbaren RTMP-Stream an, erstellt einen Broadcast mit Titel, gewählter Sichtbarkeit und gewählten Auto-Start/Stop-Werten, bindet ihn und kontrolliert die Bindung per `list`, bevor er `Vorbereitet` plus Ingest-Zugang zurückgibt.
- REQ-03: `status` unterscheidet `Inaktiv`, `Vorbereitet`, `Sendet`, `Live`, `Beendet`, `Blockiert`, `Fehler` und `Unklar`; `Live` nur bei `lifeCycleStatus=live` und passender `boundStreamId`.
- REQ-04: `start` schaltet bei `auto_start=false` nur bei `streamStatus=active` per Transition live; bei `auto_start=true` ist `start` ein Statusabruf.
- REQ-05: `finish` verlangt einen ausdrücklichen `Endegrund`; `medien_unterbrochen` beendet nichts bei YouTube.
- REQ-06: Jeder POST wird vorher als offener Schritt gespeichert; nach Prozessneustart gleicht der Adapter offene Schritte per `list` ab und wiederholt keinen POST blind. Mehrdeutige Zuordnung ergibt `Blockiert(UnklareZuordnung)`.
- REQ-07: Fremde Kanal-ID, veraltete oder fremde Verbindungsgeneration lösen keinen YouTube-Aufruf aus.
- REQ-08: Fehlende Rechte, Live nicht freigeschaltet, Quota, Ratelimit und Transportfehler werden getrennt gemeldet; Quota und Transport sind wiederaufnehmbar.
- REQ-09: Ein abgeschlossener Broadcast wird nie wieder als startfähig verwendet; ein neuer `prepare` erzeugt einen neuen Run.
- REQ-10: Titel und Beschreibung enthalten keine Run-, Session- oder sonstigen technischen Kennungen.

## Invarianten (darf sich nicht ändern)

- INV-01: Tokens kommen ausschließlich über `uplink_chat::token::TokenQuelle`; keine zweite Tokenablage, kein Speichern von Access-Token oder `streamName`.
- INV-02: Keine Änderung an `crates/uplink-service/src/**`, `crates/uplink-ingest`, `crates/uplink-media`, `crates/uplink-tls-provider`, `crates/uplink-chat/src/**`, `deployment/**`, `.github/**`, bestehenden Migrationen.
- INV-03: Bestehende Tests bleiben unverändert und grün.
- INV-04: Tests nur mit synthetischen Zugangsdaten und lokalen HTTP-Gegenstellen (wiremock); Store-Tests gegen isolierte PostgreSQL-16-Instanz.
- INV-05: Keine ENV-Konfiguration, keine Secrets in Logs, Fehlertexten oder Debug-Ausgaben.

## Nicht-Ziele

- VOD, `videos.insert`, Archiv, Google-Drive-Fallback, Social-Media-Automation.
- Anbindung an Coordinator, `api.rs`, `destinations.rs`, `migrations.rs` (Codex).
- Eigener OAuth-Flow oder Refresh.

## Erlaubter Änderungsbereich

- crates/uplink-youtube-live
- Cargo.toml
- Cargo.lock
- db/migrations/20260908_youtube_live.sql
- docs/youtube-live-vertrag.md
- .tasks/2026-09-08-youtube-live-adapter

## Verbotene Änderungen

- crates/uplink-service/**, crates/uplink-chat/**, crates/uplink-ingest/**, crates/uplink-media/**, crates/uplink-tls-provider/**, crates/uplink-core/**, crates/uplink-cli/**, crates/uplink-platform-probe/**
- deployment/**, .github/**, config/**, bestehende `db/migrations/*`
- rust-toolchain.toml, Lint-Konfiguration

## Offene Produktfragen

- keine (Anfangswerte privat und Automatik aus sind im Auftrag entschieden)

## Amendments
