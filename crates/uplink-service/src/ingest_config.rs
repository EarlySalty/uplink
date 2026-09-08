use crate::config::Config;
use uplink_ingest::{IngestLimits, MediaEvent};

const GLOBAL_INGEST_BYTES: usize = 1024 * 1024 * 1024;

impl Config {
    /// Dieselben Grenzen dienen Admission und Validierung. Auch anonyme
    /// Verbindungen können Parserpuffer füllen; sie zählen deshalb vollständig.
    pub fn ingest_limits(&self) -> Result<IngestLimits, &'static str> {
        let mut limits = IngestLimits::local_probe();
        limits.max_connections = self.max_sessions;
        limits.max_pending_connections = self.max_pending_connections;
        limits.max_event_bytes = self.media.max_event_bytes;
        limits.max_header_bytes = (64 * 1024).min(limits.max_event_bytes);
        limits.max_queued_bytes = self.media.max_queued_bytes;
        limits.max_queued_events = self.media.max_queued_events;
        limits.max_tracks = self.media.max_tracks;
        limits.rtmp.chunk.max_message_bytes = limits.max_event_bytes;
        limits.rtmp.chunk.max_partial_bytes = limits.max_event_bytes.saturating_mul(4);
        limits.rtmp.max_read_buffer_bytes = limits.max_event_bytes.saturating_mul(2);
        let bytes = self
            .ingest_allocation_bound(&limits)
            .ok_or("Die gemeinsamen Eingangsbudgets sind zu groß.")?;
        if bytes > GLOBAL_INGEST_BYTES {
            return Err("Parser und Medienpuffer überschreiten das gemeinsame Eingangsbudget.");
        }
        Ok(limits)
    }

    fn ingest_allocation_bound(&self, limits: &IngestLimits) -> Option<usize> {
        // Begrenzte Parser, temporäre Wire-Kopie, Event-/Queueverwaltung und
        // TLS-Reserve. Encoder-/GPU-/Ausgangsspeicher sind eigene Budgets.
        let per_connection = limits
            .max_queued_bytes
            .checked_add(limits.rtmp.chunk.max_partial_bytes)?
            .checked_add(limits.rtmp.max_read_buffer_bytes)?
            .checked_add(limits.max_event_bytes)?
            .checked_add(
                limits
                    .max_queued_events
                    .checked_mul(std::mem::size_of::<MediaEvent>() + 64)?,
            )?
            .checked_add(128 * 1024)?;
        per_connection.checked_mul(
            self.max_sessions
                .checked_add(self.max_pending_connections)?,
        )
    }

    pub fn media_limits(&self) -> uplink_media::MediaLimits {
        uplink_media::MediaLimits {
            max_tag_bytes: self.media.max_event_bytes,
            queue_bytes: self.media.max_queued_bytes,
            queue_events: self.media.max_queued_events,
            ..uplink_media::MediaLimits::default()
        }
    }
}
