-- NULL bewahrt die vorhandene Instanzregel; nur eine ausdrückliche Auswahl
-- verändert das Audio-Routing beim nächsten Stream. Kein Medien-/Tokenkopieren.
ALTER TABLE relay.destinations
    ADD COLUMN IF NOT EXISTS twitch_audio_mode text
    CONSTRAINT destinations_twitch_audio_mode_valid CHECK (
        twitch_audio_mode IS NULL OR
        (platform = 'twitch' AND twitch_audio_mode IN ('live', 'separate_vod'))
    );
