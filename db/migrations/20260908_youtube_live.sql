CREATE TABLE IF NOT EXISTS relay.youtube_live_settings (
    streamer_id bigint PRIMARY KEY REFERENCES relay.users(streamer_id),
    channel_id text NOT NULL,
    connection_generation bigint NOT NULL CHECK (connection_generation >= 0),
    titel text NOT NULL,
    sichtbarkeit text NOT NULL CHECK (sichtbarkeit IN ('private', 'unlisted', 'public')),
    auto_start boolean NOT NULL,
    auto_stop boolean NOT NULL,
    live_freigegeben_at timestamptz,
    stream_id text,
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE TABLE IF NOT EXISTS relay.youtube_live_runs (
    run_id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    streamer_id bigint NOT NULL REFERENCES relay.users(streamer_id),
    channel_id text NOT NULL,
    connection_generation bigint NOT NULL CHECK (connection_generation >= 0),
    uplink_session text NOT NULL,
    zustand text NOT NULL CHECK (zustand IN ('vorbereitung', 'vorbereitet', 'sendet', 'live', 'beendet')),
    schritt text CHECK (schritt IS NULL OR schritt IN ('stream_insert', 'broadcast_insert', 'bind', 'transition_live', 'transition_complete')),
    schritt_seit timestamptz,
    stream_id text,
    broadcast_id text,
    titel text NOT NULL,
    sichtbarkeit text NOT NULL CHECK (sichtbarkeit IN ('private', 'unlisted', 'public')),
    auto_start boolean NOT NULL,
    auto_stop boolean NOT NULL,
    fehler text,
    ende_grund text CHECK (ende_grund IS NULL OR ende_grund IN ('nutzer_stop', 'session_ende_bestaetigt', 'admin_stop', 'generation_ueberholt')),
    youtube_bestaetigt boolean,
    unterbrochen_at timestamptz,
    live_seit timestamptz,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    ended_at timestamptz
);
CREATE UNIQUE INDEX IF NOT EXISTS youtube_live_runs_aktiv ON relay.youtube_live_runs (streamer_id) WHERE ended_at IS NULL;
