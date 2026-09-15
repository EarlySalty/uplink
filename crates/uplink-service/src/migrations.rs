//! Begrenzter Release-Schritt, ohne Listener und ohne SQL aus Eingaben.
use crate::store::Store;

pub async fn apply(store: &Store) -> Result<(), &'static str> {
    for (name, statement) in [
        (
            "20260908_destination_fences",
            include_str!("../../../db/migrations/20260908_destination_fences.sql"),
        ),
        (
            "20260908_twitch_audio_mode",
            include_str!("../../../db/migrations/20260908_twitch_audio_mode.sql"),
        ),
        (
            "20260909_hochkant_layouts",
            include_str!("../../../db/migrations/20260909_hochkant_layouts.sql"),
        ),
        (
            "20260909_hochkant_destinations",
            include_str!("../../../db/migrations/20260909_hochkant_destinations.sql"),
        ),
        (
            "20260910_twitch_output_mode",
            include_str!("../../../db/migrations/20260910_twitch_output_mode.sql"),
        ),
        (
            "20260912_cast_sources",
            include_str!("../../../db/migrations/20260912_cast_sources.sql"),
        ),
        (
            "20260912_cast_scenes",
            include_str!("../../../db/migrations/20260912_cast_scenes.sql"),
        ),
        (
            "20260912_cast_state",
            include_str!("../../../db/migrations/20260912_cast_state.sql"),
        ),
        (
            "20260912_cast_indexes",
            include_str!("../../../db/migrations/20260912_cast_indexes.sql"),
        ),
    ] {
        store.migrate(name, statement).await?;
    }
    Ok(())
}
