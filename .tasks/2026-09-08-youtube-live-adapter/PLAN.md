# Plan: YouTube-Live-Adapter

status: aktiv
datum: 2026-09-08
contract: CONTRACT.md

Ziel und Anforderungen stehen im Contract, die Schnittstelle in `docs/youtube-live-vertrag.md`.

## M1: Crate-Gerüst, Modelle, Migration

- Änderungen: `crates/uplink-youtube-live/{Cargo.toml,src/lib.rs,src/model.rs,src/fehler.rs}`, Workspace-Eintrag, `db/migrations/20260908_youtube_live.sql`.
- Zwischenzustand: `cargo check -p uplink-youtube-live` grün, Migration parsebar (Store-Test in M3).
- Validierung: `cargo check --locked --jobs 2 -p uplink-youtube-live`.
- Stop-Regel: Workspace baut nicht mehr oder Cargo.lock ändert fremde Versionen.

## M2: YouTube-API-Client mit wiremock-Tests

- Änderungen: `src/api.rs` (Trait `LiveApi`, `GoogleLiveApi`), Fehlerabbildung (`ApiFehler`), begrenztes JSON-Lesen.
- Zwischenzustand: insert/list/bind/transition gegen wiremock; 401, 403 (Rechte, Quota, liveStreamingNotEnabled), 429, 5xx, Timeout getrennt erkannt.
- Validierung: `cargo test --locked --jobs 2 -p uplink-youtube-live api::`.
- Stop-Regel: ein Test braucht echte Google-Endpunkte oder echte Zugangsdaten.

## M3: RunStore mit isolierter PostgreSQL-Regression

- Änderungen: `src/store.rs` (`RunStore`, `SqlZugang`, `PostgresRunStore`), `tests/store_postgres.rs` mit eigener PG-16-Testinstanz.
- Zwischenzustand: Einstellungen speichern/laden, Run anlegen, CAS-Übergänge, offener Schritt setzen/löschen, Teilindex verhindert zweiten aktiven Run.
- Validierung: `cargo test --locked --jobs 2 -p uplink-youtube-live --test store_postgres -- --include-ignored`.
- Stop-Regel: ein Übergang gelingt ohne passende Vorbedingung (CAS verletzt).

## M4: Adapter prepare/start/status/medien_unterbrochen/finish

- Änderungen: `src/adapter.rs`, `tests/adapter.rs` (wiremock plus In-Memory-`RunStore`).
- Zwischenzustand: Tests für Create/Bind/Start/Stop/erneuten Start, Rechte/Quota/Transport, unklaren POST je Schritt, Neustart, fremde Kanal-ID und fremde Generation, keine Kennungen im Titel.
- Validierung: `cargo test --locked --jobs 2 -p uplink-youtube-live`, `cargo clippy --locked --jobs 2 -p uplink-youtube-live --all-targets`.
- Stop-Regel: ein Pfad wiederholt einen POST ohne vorherigen `list`-Abgleich.

## M5: Autor-Gate, Commit, Push

- `gate_hook.py --review --repo <worktree> --base da83eb3e`, Befunde beheben, Commits auf `feat/youtube-live-lifecycle`, Push.
- Stop-Regel: Gate meldet Scope-Verstoß außerhalb des Contracts.
