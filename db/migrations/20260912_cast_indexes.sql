CREATE INDEX IF NOT EXISTS cast_scenes_order_idx
    ON relay.cast_scenes(streamer_id, sort_order, scene_id);
