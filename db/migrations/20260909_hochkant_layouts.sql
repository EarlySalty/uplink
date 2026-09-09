-- Hochkantwahl je Streamer: normiertes Layout, unveränderliche positive Revision.
-- Formatversion ist Teil des JSON und keine Revision; Speichern legt immer
-- revision = max + 1 an. Es gibt keine Löschung.
CREATE TABLE IF NOT EXISTS relay.hochkant_layouts (
    layout_id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    streamer_id bigint NOT NULL REFERENCES relay.users(streamer_id),
    revision bigint NOT NULL CHECK (revision > 0),
    layout jsonb NOT NULL,
    quelle_breite integer,
    quelle_hoehe integer,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (streamer_id, revision)
);
ALTER TABLE relay.destinations ADD COLUMN IF NOT EXISTS hochkant_enabled boolean NOT NULL DEFAULT false;
ALTER TABLE relay.destinations ADD COLUMN IF NOT EXISTS hochkant_width integer;
ALTER TABLE relay.destinations ADD COLUMN IF NOT EXISTS hochkant_height integer;
