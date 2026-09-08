-- Dauerhafte Fence bleibt nach Löschen des Ziels bestehen.
-- Generation 0 ist ausschließlich der unveränderte Bestandsstand.
CREATE TABLE IF NOT EXISTS relay.destination_fences (
    streamer_id bigint NOT NULL REFERENCES relay.users(streamer_id),
    platform text NOT NULL CHECK (platform IN ('twitch','kick','youtube','tiktok')),
    generation bigint NOT NULL CHECK (generation >= 0),
    deleted boolean NOT NULL,
    PRIMARY KEY (streamer_id, platform)
);
