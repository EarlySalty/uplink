-- Eigener Betriebsmodus fuer nativen Twitch-2K-Hybrid.
ALTER TABLE relay.destinations
    DROP CONSTRAINT IF EXISTS destinations_twitch_output_mode_valid,
    ADD CONSTRAINT destinations_twitch_output_mode_valid CHECK (
        twitch_output_mode = 'single' OR
        (platform = 'twitch' AND twitch_output_mode IN ('enhanced', 'native_2k'))
    );
