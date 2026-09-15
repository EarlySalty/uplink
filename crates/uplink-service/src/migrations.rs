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
            "20260915_twitch_native_2k_hardware",
            include_str!("../../../db/migrations/20260915_twitch_native_2k_hardware.sql"),
        ),
        (
            "20260915_twitch_native_2k_mode",
            include_str!("../../../db/migrations/20260915_twitch_native_2k_mode.sql"),
        ),
    ] {
        store.migrate(name, statement).await?;
    }
    Ok(())
}
