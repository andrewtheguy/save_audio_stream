use crossbeam_channel::Receiver;
use log::warn;
use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use symphonia::core::io::MediaSource;

/// A streaming media source that reads from a channel
pub struct StreamingSource {
    receiver: Receiver<Vec<u8>>,
    buffer: Vec<u8>,
    position: usize,
    total_bytes: Arc<AtomicU64>,
    is_finished: bool,
}

impl StreamingSource {
    pub fn new(receiver: Receiver<Vec<u8>>, total_bytes: Arc<AtomicU64>) -> Self {
        Self {
            receiver,
            buffer: Vec::new(),
            position: 0,
            total_bytes,
            is_finished: false,
        }
    }

    fn fill_buffer(&mut self) {
        // Try to receive more data without blocking if buffer is getting low
        while self.position >= self.buffer.len() && !self.is_finished {
            match self.receiver.recv() {
                Ok(chunk) => {
                    if chunk.is_empty() {
                        self.is_finished = true;
                        break;
                    }
                    // Reset buffer with new chunk
                    self.buffer = chunk;
                    self.position = 0;
                }
                Err(e) => {
                    // Channel closed - this is expected when sender finishes
                    warn!("Streaming channel closed: {}", e);
                    self.is_finished = true;
                    break;
                }
            }
        }
    }
}

impl Read for StreamingSource {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.fill_buffer();

        if self.position >= self.buffer.len() {
            return Ok(0); // EOF
        }

        let available = self.buffer.len() - self.position;
        let to_read = std::cmp::min(available, buf.len());
        buf[..to_read].copy_from_slice(&self.buffer[self.position..self.position + to_read]);
        self.position += to_read;

        Ok(to_read)
    }
}

impl Seek for StreamingSource {
    fn seek(&mut self, _pos: SeekFrom) -> std::io::Result<u64> {
        // Streaming source doesn't support seeking
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Seeking not supported for streaming source",
        ))
    }
}

impl MediaSource for StreamingSource {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        // Return current known length
        Some(self.total_bytes.load(Ordering::Relaxed))
    }
}
