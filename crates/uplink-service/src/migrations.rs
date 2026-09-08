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
    ] {
        store.migrate(name, statement).await?;
    }
    Ok(())
}
