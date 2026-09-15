CREATE TABLE IF NOT EXISTS relay.cast_state (
    streamer_id bigint PRIMARY KEY REFERENCES relay.users(streamer_id) ON DELETE CASCADE,
    program_scene_id bigint,
    preview_scene_id bigint,
    switch_generation bigint NOT NULL DEFAULT 0 CHECK (switch_generation >= 0),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT cast_state_program_scene_fk
        FOREIGN KEY (streamer_id, program_scene_id)
        REFERENCES relay.cast_scenes(streamer_id, scene_id),
    CONSTRAINT cast_state_preview_scene_fk
        FOREIGN KEY (streamer_id, preview_scene_id)
        REFERENCES relay.cast_scenes(streamer_id, scene_id)
);
