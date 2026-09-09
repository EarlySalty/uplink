-- Die Hochkantspalten je Ziel bleiben nach dem Bestand unverändert: aus ist
-- der Default, eine Zielgröße wird erst mit einer freigegebenen Bildgestaltung
-- gesetzt.
ALTER TABLE relay.destinations
    ADD COLUMN IF NOT EXISTS hochkant_enabled boolean NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS hochkant_width integer,
    ADD COLUMN IF NOT EXISTS hochkant_height integer;
