-- Bestehende Ziele behalten den bisherigen Einzelstream. Enhanced muss der
-- Streamer ausdrücklich auswählen; die aktive Betriebsart ist Laufzeitstatus.
ALTER TABLE relay.destinations
    ADD COLUMN IF NOT EXISTS twitch_output_mode text NOT NULL DEFAULT 'single'
    CONSTRAINT destinations_twitch_output_mode_valid CHECK (
        twitch_output_mode = 'single' OR
        (platform = 'twitch' AND twitch_output_mode = 'enhanced')
    );
