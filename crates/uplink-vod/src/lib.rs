//! Dauerhafte VOD-Aufträge. Keine OAuth- oder Secretablage neben dem Dienstbroker.
mod model;
pub mod youtube;
pub use model::*;
pub mod store;
pub use store::{Database, JobStatus, Store};
pub mod storage;
pub use storage::{ExportFile, Storage};
pub mod recording;
pub mod worker;
pub use worker::{PreparedSource, SourceProvider, SourceRequest, Worker};
pub mod sources;
pub use recording::RecordingManifest;
pub use sources::{NativeSources, SourceConfig};
