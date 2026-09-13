# Evidence: YouTube-Live-Adapter für Uplink

status: aktiv
datum: 2026-09-08
contract: CONTRACT.md

## Analoge Implementierungen (wie löst das Repo so etwas schon?)

- crates/uplink-chat/src/youtube.rs:34: `YouTubeEndpunkte` mit injizierbarer API-Basis und Takten; gleiches Muster für den Live-Adapter (Basis-URL für wiremock).
- crates/uplink-chat/src/youtube.rs:54: `YouTubeFabrik` hält `Arc<TokenQuelle>` und einen reqwest-Client mit `no_proxy`, Frist 10 s, keine Redirects; wird übernommen.
- crates/uplink-chat/src/youtube.rs:713: wiremock-Tests mit `bot_mit_youtube_token` (synthetischer Grant `UC-kanal`, Scope `youtube.force-ssl`); Vorlage für die Adaptertests.
- crates/uplink-service/src/chat.rs:121: `BotBroker` als `PlatformBroker` mit Fehlercodes `connection_missing`, `reauthorization_required`, `identity_mismatch`, `missing_scope`; der Adapter baut keinen zweiten Broker.

## Bestehende Abstraktionen (werden wiederverwendet, nicht nachgebaut)

- crates/uplink-chat/src/token.rs:49: Trait `PlatformBroker::grant(streamer_id, platform, refresh)`.
- crates/uplink-chat/src/token.rs:126: `TokenQuelle` (Cache, Invalidierung, Generation je Anfrage); einziger Tokenweg.
- crates/uplink-chat/src/token.rs:173: `TokenQuelle::zugang(id, Platform)` liefert `Grant` mit `platform_user_id` und `scopes`.
- crates/uplink-chat/src/token.rs:281: `TokenQuelle::invalidieren` nach 401.
- crates/uplink-chat/src/lib.rs:25: `Platform::YouTube`, `as_str()` = `youtube`.
- crates/uplink-service/src/store.rs:88: `Store::query(sql, params)`, eine Transaktion je Aufruf mit Frist; der Adapter definiert dafür den Trait `SqlZugang`.
- crates/uplink-service/src/store.rs:60: `Store::migrate(name, sql)` prüft Checksumme; Migrationsname nur `[A-Za-z0-9_]`.

## Relevante Tests (laufen vorher, laufen nachher)

- crates/uplink-service/tests/support/database.rs:12: `Database::start()` startet PostgreSQL 16 aus `/usr/lib/postgresql/16/bin` unter `/tmp`, Unix-Socket, `max_connections=10`; Muster für den isolierten Store-Test des Adapters.
- crates/uplink-service/tests/database.rs:40: `#[ignore = "Benötigt isolierte PostgreSQL-16-Testinstanz."]`; gleiche Kennzeichnung für die Adapter-Store-Tests.
- crates/uplink-chat/src/youtube.rs:752: bestehender YouTube-Chat-Test `ohne_broadcast_bleibt_getrennt_und_wartet` bleibt unverändert grün.

## Öffentliche Schnittstellen und Verträge (dürfen nicht brechen)

- db/migrations/20260908_destination_fences.sql:3: `relay.destination_fences(streamer_id, platform, generation, deleted)`; Generation des YouTube-Ziels für `Identitaet.connection_generation`.
- crates/uplink-service/src/migrations.rs:4: Registrierung per `include_str!`; die neue Migration wird dort erst durch Codex eingetragen.
- crates/uplink-chat/Cargo.toml:8: Abhängigkeiten `reqwest 0.12` (rustls), `wiremock 0.6`, `thiserror 2`, `zeroize 1`; gleiche Versionen im neuen Crate.
- crates/uplink-chat/src/http.rs:20: begrenztes JSON-Lesen (`bytes`, `json`); das Modul ist privat, der Adapter baut eine eigene begrenzte Variante (256 KiB).

## Änderungsfläche (welche Dateien voraussichtlich angefasst werden)

- crates/uplink-youtube-live/Cargo.toml, src/{lib,model,api,store,adapter,fehler}.rs, tests/*.rs: neues Crate.
- Cargo.toml: Workspace-Member `crates/uplink-youtube-live`.
- Cargo.lock: neue Abhängigkeitsauflösung.
- db/migrations/20260908_youtube_live.sql: zwei neue Tabellen im Schema `relay`.
- docs/youtube-live-vertrag.md: Schnittstellenvertrag.

## Offene Architekturfrage

- keine
