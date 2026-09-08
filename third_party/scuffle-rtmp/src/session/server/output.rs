// Local Uplink fork, modified 2026-09-08; see PATCHES.md for provenance and scope.
use std::io;

pub(super) struct BoundedWriter<'a> {
    pub(super) buffer: &'a mut Vec<u8>,
    pub(super) limit: usize,
}

impl io::Write for BoundedWriter<'_> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.len() > self.limit.saturating_sub(self.buffer.len()) {
            return Err(io::Error::other("RTMP output buffer limit"));
        }
        self.buffer
            .try_reserve_exact(data.len())
            .map_err(|_| io::Error::other("RTMP output allocation failed"))?;
        self.buffer.extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
