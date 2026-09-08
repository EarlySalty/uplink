-- Getrennte Quellen, dauerhafte Sperren und dieselbe Sperrreihenfolge für
-- Bindungsänderungen sowie Upload-/Aufräumwirkungen.
ALTER TABLE relay.vod_objects ADD COLUMN IF NOT EXISTS source text NOT NULL DEFAULT 'input_recording'
    CHECK(source IN ('input_recording','twitch_vod'));
UPDATE relay.vod_objects SET source='twitch_vod' WHERE manifest->>'source'='twitch_vod';
ALTER TABLE relay.vod_objects DROP CONSTRAINT IF EXISTS vod_objects_session_id_key;
CREATE UNIQUE INDEX IF NOT EXISTS vod_objects_session_source ON relay.vod_objects(session_id,source);
ALTER TABLE relay.vod_jobs ADD COLUMN IF NOT EXISTS proof_blocked boolean NOT NULL DEFAULT false;
ALTER TABLE relay.vod_jobs ADD COLUMN IF NOT EXISTS publication_confirmed boolean NOT NULL DEFAULT false;
ALTER TABLE relay.vod_jobs DROP CONSTRAINT IF EXISTS vod_jobs_state_check;
ALTER TABLE relay.vod_jobs ADD CONSTRAINT vod_jobs_state_check CHECK(state IN ('waiting_source','prepared','starting','uploading','processing','publishing','ready','blocked','failed','cancelled'));
DO $$ BEGIN
    IF NOT EXISTS(SELECT 1 FROM pg_constraint WHERE conrelid='relay.vod_jobs'::regclass AND conname='vod_ready_has_total') THEN
        ALTER TABLE relay.vod_jobs ADD CONSTRAINT vod_ready_has_total CHECK
            (state<>'ready' OR (total_bytes IS NOT NULL AND (settings->>'privacy'='private' OR publication_confirmed)));
    END IF;
END $$;
CREATE INDEX IF NOT EXISTS vod_jobs_streamer_id ON relay.vod_jobs(streamer_id,id DESC);
CREATE INDEX IF NOT EXISTS vod_objects_streamer_id ON relay.vod_objects(streamer_id);
CREATE INDEX IF NOT EXISTS vod_twitch_bindings_streamer_id ON relay.vod_twitch_bindings(streamer_id);

CREATE OR REPLACE FUNCTION relay.vod_binding_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM 1 FROM relay.sessions WHERE id=NEW.session_id AND streamer_id=NEW.streamer_id FOR UPDATE;
    IF NOT FOUND THEN RAISE EXCEPTION 'VOD session identity mismatch' USING ERRCODE='23514'; END IF;
    IF TG_OP='UPDATE' THEN
        IF OLD.session_id<>NEW.session_id OR OLD.streamer_id<>NEW.streamer_id THEN
            RAISE EXCEPTION 'VOD binding identity is immutable' USING ERRCODE='23514';
        END IF;
        NEW.conflicted := OLD.conflicted OR NEW.conflicted
            OR OLD.binding->>'stream_id' IS DISTINCT FROM NEW.binding->>'stream_id'
            OR OLD.binding->>'broadcaster_id' IS DISTINCT FROM NEW.binding->>'broadcaster_id';
        NEW.binding := jsonb_set(NEW.binding,'{vod_audio_confirmed}',to_jsonb(
            COALESCE((OLD.binding->>'vod_audio_confirmed')::boolean,false)
            AND COALESCE((NEW.binding->>'vod_audio_confirmed')::boolean,false)));
    END IF;
    IF NEW.conflicted OR NOT COALESCE((NEW.binding->>'vod_audio_confirmed')::boolean,false) THEN
        UPDATE relay.vod_jobs SET proof_blocked=true,state='blocked',resume_state=NULL,
            last_error=CASE WHEN NEW.conflicted THEN 'unbound_twitch' ELSE 'missing_vod_audio' END,
            lease_owner=NULL,lease_until=NULL,updated_at=now()
        WHERE session_id=NEW.session_id AND streamer_id=NEW.streamer_id AND settings->>'source'='twitch_vod';
    END IF;
    RETURN NEW;
END $$;
DROP TRIGGER IF EXISTS vod_binding_guard ON relay.vod_twitch_bindings;
CREATE TRIGGER vod_binding_guard BEFORE INSERT OR UPDATE ON relay.vod_twitch_bindings
    FOR EACH ROW EXECUTE FUNCTION relay.vod_binding_guard();

CREATE OR REPLACE FUNCTION relay.vod_job_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE binding_row relay.vod_twitch_bindings%ROWTYPE; object_state text;
BEGIN
    IF TG_OP='UPDATE' AND (NEW.settings IS DISTINCT FROM OLD.settings OR NEW.session_id<>OLD.session_id OR NEW.streamer_id<>OLD.streamer_id) THEN
        RAISE EXCEPTION 'VOD job snapshot is immutable' USING ERRCODE='23514';
    END IF;
    -- Auch direkte Schreibpfade dürfen eine vorher dauerhaft gespeicherte Sperre
    -- nicht durch einen späteren Status- oder Retry-Übergang zurücksetzen.
    IF TG_OP='UPDATE' AND OLD.proof_blocked AND
        (NOT NEW.proof_blocked OR NEW.state NOT IN ('blocked','failed','cancelled')) THEN
        RAISE EXCEPTION 'VOD proof remains blocked' USING ERRCODE='23514';
    END IF;
    IF NEW.settings->>'source'='twitch_vod' THEN
        SELECT * INTO binding_row FROM relay.vod_twitch_bindings
            WHERE session_id=NEW.session_id AND streamer_id=NEW.streamer_id;
        IF FOUND AND (binding_row.conflicted OR NOT COALESCE((binding_row.binding->>'vod_audio_confirmed')::boolean,false)) THEN
            NEW.proof_blocked:=true; NEW.state:='blocked'; NEW.resume_state:=NULL;
            NEW.last_error:=CASE WHEN binding_row.conflicted THEN 'unbound_twitch' ELSE 'missing_vod_audio' END;
            NEW.lease_owner:=NULL; NEW.lease_until:=NULL;
        ELSIF NOT FOUND AND NEW.state NOT IN ('waiting_source','blocked','failed','cancelled') THEN
            RAISE EXCEPTION 'VOD binding missing' USING ERRCODE='23514';
        END IF;
    END IF;
    IF NEW.object_id IS NOT NULL AND (TG_OP='INSERT' OR OLD.object_id IS DISTINCT FROM NEW.object_id) THEN
        SELECT state INTO object_state FROM relay.vod_objects WHERE id=NEW.object_id AND streamer_id=NEW.streamer_id FOR UPDATE;
        IF object_state IS DISTINCT FROM 'available' THEN
            RAISE EXCEPTION 'VOD object is unavailable' USING ERRCODE='23514';
        END IF;
    END IF;
    RETURN NEW;
END $$;
DROP TRIGGER IF EXISTS vod_job_guard ON relay.vod_jobs;
CREATE TRIGGER vod_job_guard BEFORE INSERT OR UPDATE ON relay.vod_jobs
    FOR EACH ROW EXECUTE FUNCTION relay.vod_job_guard();
