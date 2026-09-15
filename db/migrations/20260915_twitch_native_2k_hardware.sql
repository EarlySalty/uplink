-- Quellrechner-Hardware fuer echten Twitch-2K-Hybrid.
-- Das JSON wird ausschliesslich nach Rust-Schemavalidierung geschrieben und vor
-- jedem GoLive-Aufruf erneut validiert. Keine Zugangsdaten gehoeren hinein.
ALTER TABLE relay.users
    ADD COLUMN IF NOT EXISTS twitch_native_2k_hardware jsonb;
