-- Additiv. relay.users und relay.sessions stammen aus dem Dienst.
CREATE UNIQUE INDEX IF NOT EXISTS vod_sessions_identity ON relay.sessions(id,streamer_id);
CREATE TABLE IF NOT EXISTS relay.vod_settings (
    streamer_id bigint PRIMARY KEY REFERENCES relay.users(streamer_id),
    settings jsonb NOT NULL CHECK (jsonb_typeof(settings) = 'object'),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS relay.vod_twitch_bindings (
    session_id bigint PRIMARY KEY REFERENCES relay.sessions(id),
    streamer_id bigint NOT NULL REFERENCES relay.users(streamer_id),
    binding jsonb NOT NULL CHECK (jsonb_typeof(binding) = 'object'),
    conflicted boolean NOT NULL DEFAULT false,
    FOREIGN KEY(session_id,streamer_id) REFERENCES relay.sessions(id,streamer_id)
);
CREATE TABLE IF NOT EXISTS relay.vod_objects (
    id text PRIMARY KEY CHECK (id ~ '^[a-f0-9]{32}$'),
    session_id bigint NOT NULL UNIQUE REFERENCES relay.sessions(id),
    streamer_id bigint NOT NULL REFERENCES relay.users(streamer_id),
    manifest jsonb NOT NULL,
    complete boolean NOT NULL DEFAULT false,
    state text NOT NULL DEFAULT 'available' CHECK (state IN ('available','deleting','deleted')),
    cleanup_owner text,
    cleanup_until timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((cleanup_owner IS NULL) = (cleanup_until IS NULL)),
    UNIQUE(id,streamer_id),
    FOREIGN KEY(session_id,streamer_id) REFERENCES relay.sessions(id,streamer_id)
);
CREATE TABLE IF NOT EXISTS relay.vod_jobs (
    id bigserial PRIMARY KEY,
    session_id bigint NOT NULL UNIQUE REFERENCES relay.sessions(id),
    streamer_id bigint NOT NULL REFERENCES relay.users(streamer_id),
    ended_at timestamptz NOT NULL,
    end_reason text NOT NULL CHECK (end_reason IN ('explicit_stop','reconnect_expired','controlled_stop')),
    settings jsonb NOT NULL,
    state text NOT NULL DEFAULT 'waiting_source' CHECK (state IN ('waiting_source','prepared','starting','uploading','processing','ready','blocked','failed','cancelled')),
    resume_state text,
    object_id text,
    upload_session_enc bytea,
    confirmed_bytes bigint NOT NULL DEFAULT 0 CHECK (confirmed_bytes>=0),
    total_bytes bigint CHECK (total_bytes>0),
    video_id text,
    processing_succeeded boolean NOT NULL DEFAULT false,
    last_error text,
    next_attempt_at timestamptz NOT NULL DEFAULT now(),
    lease_owner text,
    lease_until timestamptz,
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts>=0),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (total_bytes IS NULL OR confirmed_bytes<=total_bytes),
    CHECK (NOT processing_succeeded OR video_id IS NOT NULL),
    CHECK (state <> 'ready' OR (processing_succeeded AND video_id IS NOT NULL AND confirmed_bytes=total_bytes)),
    CHECK ((lease_owner IS NULL) = (lease_until IS NULL)),
    FOREIGN KEY(session_id,streamer_id) REFERENCES relay.sessions(id,streamer_id),
    FOREIGN KEY(object_id,streamer_id) REFERENCES relay.vod_objects(id,streamer_id)
);
CREATE INDEX IF NOT EXISTS vod_jobs_due ON relay.vod_jobs(next_attempt_at,id) WHERE state NOT IN ('ready','blocked','failed','cancelled');
CREATE INDEX IF NOT EXISTS vod_jobs_object ON relay.vod_jobs(object_id);
