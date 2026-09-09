use crate::SessionScope;
use std::{collections::VecDeque, time::Duration};

const TIMESTAMP_TOLERANCE_MS: u64 = 2_000;
const TIMESTAMP_CLAMP_LIMIT: usize = 50;
const TIMESTAMP_CLAMP_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy)]
pub struct BufferLimits {
    pub max_bytes: usize,
    pub max_packets: usize,
    pub max_age: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub scope: SessionScope,
    pub track_id: u32,
    pub decode_timestamp_ms: u64,
    pub payload: Box<[u8]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferError {
    InvalidLimits,
    EmptyPacket,
    WrongScope,
    WrongTrack,
    PacketTooLarge,
    Full,
    ClockRegression,
    TimestampRegression,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushResult {
    pub expired_packets: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PopResult {
    pub expired_packets: usize,
    pub packet: Option<Packet>,
}

/// Begrenzte FIFO für bereits komprimierte Pakete EINER Sessiongeneration/Spur.
/// Kein GOP-Cache, Jitter-Regler, Decoder oder persistenter Delay-Speicher.
/// Ablauf wird bei Zugriff entfernt und als Verlust gezählt; der Caller muss
/// nach Verlust einen gültigen Decoder-Einstieg organisieren.
pub struct PacketBuffer {
    scope: SessionScope,
    track_id: u32,
    limits: BufferLimits,
    packets: VecDeque<(Duration, Packet)>,
    bytes: usize,
    last_now: Duration,
    last_dts: Option<u64>,
    expired_total: u64,
    timestamp_clamps: VecDeque<Duration>,
}

impl PacketBuffer {
    pub fn new(
        scope: SessionScope,
        track_id: u32,
        limits: BufferLimits,
    ) -> Result<Self, BufferError> {
        if track_id == 0
            || limits.max_bytes == 0
            || limits.max_packets == 0
            || limits.max_age.is_zero()
        {
            return Err(BufferError::InvalidLimits);
        }
        Ok(Self {
            scope,
            track_id,
            limits,
            packets: VecDeque::new(),
            bytes: 0,
            last_now: Duration::ZERO,
            last_dts: None,
            expired_total: 0,
            timestamp_clamps: VecDeque::new(),
        })
    }

    /// Aktuell allokierte Nutzdaten, auch wenn ein inaktiver Puffer alte Pakete hält.
    /// `expire` kann vom Worker-Timer aufgerufen werden.
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    pub fn packet_count(&self) -> usize {
        self.packets.len()
    }
    pub fn expired_total(&self) -> u64 {
        self.expired_total
    }

    pub fn expire(&mut self, now: Duration) -> Result<usize, BufferError> {
        if now < self.last_now {
            return Err(BufferError::ClockRegression);
        }
        self.last_now = now;
        let mut expired = 0;
        while self
            .packets
            .front()
            .is_some_and(|(arrival, _)| now - *arrival >= self.limits.max_age)
        {
            if let Some((_, packet)) = self.packets.pop_front() {
                self.bytes -= packet.payload.len();
                expired += 1;
            }
        }
        self.expired_total = self.expired_total.saturating_add(expired as u64);
        Ok(expired)
    }

    pub fn push(&mut self, mut packet: Packet, now: Duration) -> Result<PushResult, BufferError> {
        if packet.scope != self.scope {
            return Err(BufferError::WrongScope);
        }
        if packet.track_id != self.track_id {
            return Err(BufferError::WrongTrack);
        }
        if packet.payload.is_empty() {
            return Err(BufferError::EmptyPacket);
        }
        if packet.payload.len() > self.limits.max_bytes {
            return Err(BufferError::PacketTooLarge);
        }
        if now < self.last_now {
            return Err(BufferError::ClockRegression);
        }
        let regression = self
            .last_dts
            .filter(|last| packet.decode_timestamp_ms < *last);
        if let Some(last) = regression {
            while self
                .timestamp_clamps
                .front()
                .is_some_and(|at| now - *at >= TIMESTAMP_CLAMP_WINDOW)
            {
                self.timestamp_clamps.pop_front();
            }
            if last - packet.decode_timestamp_ms > TIMESTAMP_TOLERANCE_MS
                || self.timestamp_clamps.len() >= TIMESTAMP_CLAMP_LIMIT
            {
                return Err(BufferError::TimestampRegression);
            }
            packet.decode_timestamp_ms = last;
        }
        let expired_packets = self.expire(now)?;
        if self.packets.len() >= self.limits.max_packets
            || packet.payload.len() > self.limits.max_bytes - self.bytes
        {
            return Err(BufferError::Full);
        }
        if regression.is_some() {
            self.timestamp_clamps.push_back(now);
        }
        self.last_dts = Some(packet.decode_timestamp_ms);
        self.bytes += packet.payload.len();
        self.packets.push_back((now, packet));
        Ok(PushResult { expired_packets })
    }

    pub fn pop(&mut self, now: Duration) -> Result<PopResult, BufferError> {
        let expired_packets = self.expire(now)?;
        let packet = self.packets.pop_front().map(|(_, packet)| packet);
        if let Some(packet) = &packet {
            self.bytes -= packet.payload.len();
        }
        Ok(PopResult {
            expired_packets,
            packet,
        })
    }
}
