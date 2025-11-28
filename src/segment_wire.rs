//! Binary wire format for segment data transfer.
//!
//! This module provides efficient binary encoding/decoding for segment data,
//! replacing JSON+base64 encoding to save ~30% bandwidth.
//!
//! ## Wire Format
//!
//! ```text
//! [Header: 16 bytes] [Segment 1] [Segment 2] ... [Segment N]
//! ```
//!
//! ### Header (16 bytes, little-endian)
//! | Offset | Size | Field |
//! |--------|------|-------|
//! | 0 | 4 | Magic: 0x53454753 ("SEGS") |
//! | 4 | 4 | Version: 1 |
//! | 8 | 4 | Segment count (u32) |
//! | 12 | 4 | CRC32 checksum (of all data after header) |
//!
//! ### Per-Segment (40 + audio_len bytes)
//! | Offset | Size | Field |
//! |--------|------|-------|
//! | 0 | 8 | id (i64) |
//! | 8 | 8 | timestamp_ms (i64) |
//! | 16 | 4 | is_timestamp_from_source (i32) |
//! | 20 | 8 | section_id (i64) |
//! | 28 | 8 | duration_samples (i64) |
//! | 36 | 4 | audio_data_len (u32) |
//! | 40 | N | audio_data (raw bytes) |

use std::fmt;

/// Magic number: "SEGS" in ASCII (little-endian)
pub const MAGIC: u32 = 0x53454753;

/// Protocol version
pub const VERSION: u32 = 1;

/// Header size in bytes
pub const HEADER_SIZE: usize = 16;

/// Per-segment header size (before audio data)
pub const SEGMENT_HEADER_SIZE: usize = 40;

/// Content-Type header value for this format
pub const CONTENT_TYPE: &str = "application/x-segment-stream";

/// Segment data for wire transfer (no serde required)
#[derive(Debug, Clone)]
pub struct WireSegment {
    pub id: i64,
    pub timestamp_ms: i64,
    pub is_timestamp_from_source: i32,
    pub audio_data: Vec<u8>,
    pub section_id: i64,
    pub duration_samples: i64,
}

/// Errors that can occur during decoding
#[derive(Debug)]
pub enum DecodeError {
    /// Magic number doesn't match expected value
    InvalidMagic { expected: u32, got: u32 },
    /// Protocol version is not supported
    UnsupportedVersion { expected: u32, got: u32 },
    /// Header is incomplete
    TruncatedHeader { expected: usize, got: usize },
    /// Segment data is incomplete
    TruncatedSegment {
        segment_index: usize,
        expected: usize,
        got: usize,
    },
    /// Audio data length exceeds available data
    InvalidAudioDataLen {
        segment_index: usize,
        claimed: u32,
        available: usize,
    },
    /// CRC32 checksum doesn't match
    ChecksumMismatch { expected: u32, computed: u32 },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::InvalidMagic { expected, got } => {
                write!(
                    f,
                    "Invalid magic number: expected 0x{:08X}, got 0x{:08X}",
                    expected, got
                )
            }
            DecodeError::UnsupportedVersion { expected, got } => {
                write!(
                    f,
                    "Unsupported protocol version: expected {}, got {}",
                    expected, got
                )
            }
            DecodeError::TruncatedHeader { expected, got } => {
                write!(
                    f,
                    "Truncated header: expected {} bytes, got {}",
                    expected, got
                )
            }
            DecodeError::TruncatedSegment {
                segment_index,
                expected,
                got,
            } => {
                write!(
                    f,
                    "Truncated segment {}: expected {} bytes, got {}",
                    segment_index, expected, got
                )
            }
            DecodeError::InvalidAudioDataLen {
                segment_index,
                claimed,
                available,
            } => {
                write!(
                    f,
                    "Segment {} claims {} bytes of audio data, but only {} available",
                    segment_index, claimed, available
                )
            }
            DecodeError::ChecksumMismatch { expected, computed } => {
                write!(
                    f,
                    "CRC32 checksum mismatch: expected 0x{:08X}, computed 0x{:08X}",
                    expected, computed
                )
            }
        }
    }
}

impl std::error::Error for DecodeError {}

/// Encode segments to binary wire format.
///
/// Returns a byte vector containing the header and all segments.
/// The CRC32 checksum covers all segment data (everything after the header).
pub fn encode_segments(segments: &[WireSegment]) -> Vec<u8> {
    // Calculate total size for pre-allocation
    let segments_size: usize = segments
        .iter()
        .map(|s| SEGMENT_HEADER_SIZE + s.audio_data.len())
        .sum();
    let total_size = HEADER_SIZE + segments_size;

    let mut buf = Vec::with_capacity(total_size);

    // Write header (CRC32 placeholder will be filled later)
    buf.extend_from_slice(&MAGIC.to_le_bytes());
    buf.extend_from_slice(&VERSION.to_le_bytes());
    buf.extend_from_slice(&(segments.len() as u32).to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes()); // CRC32 placeholder

    // Write each segment
    for seg in segments {
        buf.extend_from_slice(&seg.id.to_le_bytes());
        buf.extend_from_slice(&seg.timestamp_ms.to_le_bytes());
        buf.extend_from_slice(&seg.is_timestamp_from_source.to_le_bytes());
        buf.extend_from_slice(&seg.section_id.to_le_bytes());
        buf.extend_from_slice(&seg.duration_samples.to_le_bytes());
        buf.extend_from_slice(&(seg.audio_data.len() as u32).to_le_bytes());
        buf.extend_from_slice(&seg.audio_data);
    }

    // Compute CRC32 of segment data (everything after header)
    let crc = crc32fast::hash(&buf[HEADER_SIZE..]);

    // Write CRC32 into header at offset 12
    buf[12..16].copy_from_slice(&crc.to_le_bytes());

    buf
}

/// Decode segments from binary wire format.
///
/// Validates magic number, version, and CRC32 checksum before returning segments.
pub fn decode_segments(data: &[u8]) -> Result<Vec<WireSegment>, DecodeError> {
    // Check header size
    if data.len() < HEADER_SIZE {
        return Err(DecodeError::TruncatedHeader {
            expected: HEADER_SIZE,
            got: data.len(),
        });
    }

    // Validate magic number
    let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    if magic != MAGIC {
        return Err(DecodeError::InvalidMagic {
            expected: MAGIC,
            got: magic,
        });
    }

    // Validate version
    let version = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    if version != VERSION {
        return Err(DecodeError::UnsupportedVersion {
            expected: VERSION,
            got: version,
        });
    }

    // Read segment count and CRC32
    let count = u32::from_le_bytes([data[8], data[9], data[10], data[11]]) as usize;
    let expected_crc = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);

    // Verify CRC32 of segment data
    let computed_crc = crc32fast::hash(&data[HEADER_SIZE..]);
    if computed_crc != expected_crc {
        return Err(DecodeError::ChecksumMismatch {
            expected: expected_crc,
            computed: computed_crc,
        });
    }

    // Parse segments
    let mut segments = Vec::with_capacity(count);
    let mut pos = HEADER_SIZE;

    for i in 0..count {
        // Check segment header size
        if pos + SEGMENT_HEADER_SIZE > data.len() {
            return Err(DecodeError::TruncatedSegment {
                segment_index: i,
                expected: SEGMENT_HEADER_SIZE,
                got: data.len() - pos,
            });
        }

        let id = i64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
        let timestamp_ms = i64::from_le_bytes(data[pos + 8..pos + 16].try_into().unwrap());
        let is_timestamp_from_source =
            i32::from_le_bytes(data[pos + 16..pos + 20].try_into().unwrap());
        let section_id = i64::from_le_bytes(data[pos + 20..pos + 28].try_into().unwrap());
        let duration_samples = i64::from_le_bytes(data[pos + 28..pos + 36].try_into().unwrap());
        let audio_len = u32::from_le_bytes(data[pos + 36..pos + 40].try_into().unwrap()) as usize;

        pos += SEGMENT_HEADER_SIZE;

        // Check audio data size
        if pos + audio_len > data.len() {
            return Err(DecodeError::InvalidAudioDataLen {
                segment_index: i,
                claimed: audio_len as u32,
                available: data.len() - pos,
            });
        }

        let audio_data = data[pos..pos + audio_len].to_vec();
        pos += audio_len;

        segments.push(WireSegment {
            id,
            timestamp_ms,
            is_timestamp_from_source,
            audio_data,
            section_id,
            duration_samples,
        });
    }

    Ok(segments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode_empty() {
        let segments: Vec<WireSegment> = vec![];
        let encoded = encode_segments(&segments);
        let decoded = decode_segments(&encoded).unwrap();
        assert_eq!(decoded.len(), 0);
    }

    #[test]
    fn test_encode_decode_single_segment() {
        let segments = vec![WireSegment {
            id: 42,
            timestamp_ms: 1234567890,
            is_timestamp_from_source: 1,
            audio_data: vec![0x01, 0x02, 0x03, 0x04],
            section_id: 10,
            duration_samples: 960,
        }];

        let encoded = encode_segments(&segments);
        let decoded = decode_segments(&encoded).unwrap();

        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].id, 42);
        assert_eq!(decoded[0].timestamp_ms, 1234567890);
        assert_eq!(decoded[0].is_timestamp_from_source, 1);
        assert_eq!(decoded[0].audio_data, vec![0x01, 0x02, 0x03, 0x04]);
        assert_eq!(decoded[0].section_id, 10);
        assert_eq!(decoded[0].duration_samples, 960);
    }

    #[test]
    fn test_encode_decode_multiple_segments() {
        let segments = vec![
            WireSegment {
                id: 1,
                timestamp_ms: 1000,
                is_timestamp_from_source: 1,
                audio_data: vec![0xAA; 100],
                section_id: 1,
                duration_samples: 960,
            },
            WireSegment {
                id: 2,
                timestamp_ms: 2000,
                is_timestamp_from_source: 0,
                audio_data: vec![0xBB; 200],
                section_id: 1,
                duration_samples: 960,
            },
            WireSegment {
                id: 3,
                timestamp_ms: 3000,
                is_timestamp_from_source: 1,
                audio_data: vec![0xCC; 50],
                section_id: 2,
                duration_samples: 480,
            },
        ];

        let encoded = encode_segments(&segments);
        let decoded = decode_segments(&encoded).unwrap();

        assert_eq!(decoded.len(), 3);
        for (i, (orig, dec)) in segments.iter().zip(decoded.iter()).enumerate() {
            assert_eq!(orig.id, dec.id, "Segment {} id mismatch", i);
            assert_eq!(
                orig.timestamp_ms, dec.timestamp_ms,
                "Segment {} timestamp_ms mismatch",
                i
            );
            assert_eq!(
                orig.is_timestamp_from_source, dec.is_timestamp_from_source,
                "Segment {} is_timestamp_from_source mismatch",
                i
            );
            assert_eq!(
                orig.audio_data, dec.audio_data,
                "Segment {} audio_data mismatch",
                i
            );
            assert_eq!(
                orig.section_id, dec.section_id,
                "Segment {} section_id mismatch",
                i
            );
            assert_eq!(
                orig.duration_samples, dec.duration_samples,
                "Segment {} duration_samples mismatch",
                i
            );
        }
    }

    #[test]
    fn test_invalid_magic() {
        let mut data = encode_segments(&[]);
        data[0] = 0xFF; // Corrupt magic number
        let result = decode_segments(&data);
        assert!(matches!(result, Err(DecodeError::InvalidMagic { .. })));
    }

    #[test]
    fn test_invalid_version() {
        let mut data = encode_segments(&[]);
        data[4] = 0xFF; // Set invalid version
        // Also need to fix CRC
        let crc = crc32fast::hash(&data[HEADER_SIZE..]);
        data[12..16].copy_from_slice(&crc.to_le_bytes());

        let result = decode_segments(&data);
        assert!(matches!(result, Err(DecodeError::UnsupportedVersion { .. })));
    }

    #[test]
    fn test_checksum_mismatch() {
        let segments = vec![WireSegment {
            id: 1,
            timestamp_ms: 1000,
            is_timestamp_from_source: 1,
            audio_data: vec![0xAA; 100],
            section_id: 1,
            duration_samples: 960,
        }];

        let mut encoded = encode_segments(&segments);
        // Corrupt audio data
        encoded[HEADER_SIZE + SEGMENT_HEADER_SIZE] = 0xFF;

        let result = decode_segments(&encoded);
        assert!(matches!(result, Err(DecodeError::ChecksumMismatch { .. })));
    }

    #[test]
    fn test_truncated_header() {
        let data = vec![0u8; 8]; // Less than HEADER_SIZE
        let result = decode_segments(&data);
        assert!(matches!(result, Err(DecodeError::TruncatedHeader { .. })));
    }

    #[test]
    fn test_checksum_detects_corrupted_crc_field() {
        let segments = vec![WireSegment {
            id: 1,
            timestamp_ms: 1000,
            is_timestamp_from_source: 1,
            audio_data: vec![0xAA; 100],
            section_id: 1,
            duration_samples: 960,
        }];

        let mut encoded = encode_segments(&segments);
        // Corrupt the CRC32 field itself (bytes 12-15)
        encoded[12] ^= 0x01;

        let result = decode_segments(&encoded);
        assert!(matches!(result, Err(DecodeError::ChecksumMismatch { .. })));
    }

    #[test]
    fn test_checksum_detects_corrupted_segment_id() {
        let segments = vec![WireSegment {
            id: 12345,
            timestamp_ms: 1000,
            is_timestamp_from_source: 1,
            audio_data: vec![0xAA; 50],
            section_id: 1,
            duration_samples: 960,
        }];

        let mut encoded = encode_segments(&segments);
        // Corrupt the segment ID field (first 8 bytes after header)
        encoded[HEADER_SIZE] ^= 0x01;

        let result = decode_segments(&encoded);
        assert!(matches!(result, Err(DecodeError::ChecksumMismatch { .. })));
    }

    #[test]
    fn test_checksum_detects_corrupted_timestamp() {
        let segments = vec![WireSegment {
            id: 1,
            timestamp_ms: 9999999999,
            is_timestamp_from_source: 1,
            audio_data: vec![0xBB; 50],
            section_id: 1,
            duration_samples: 960,
        }];

        let mut encoded = encode_segments(&segments);
        // Corrupt the timestamp field (bytes 8-15 after header)
        encoded[HEADER_SIZE + 8] ^= 0xFF;

        let result = decode_segments(&encoded);
        assert!(matches!(result, Err(DecodeError::ChecksumMismatch { .. })));
    }

    #[test]
    fn test_checksum_detects_corrupted_audio_length() {
        let segments = vec![WireSegment {
            id: 1,
            timestamp_ms: 1000,
            is_timestamp_from_source: 1,
            audio_data: vec![0xCC; 100],
            section_id: 1,
            duration_samples: 960,
        }];

        let mut encoded = encode_segments(&segments);
        // Corrupt the audio_data_len field (bytes 36-39 after header)
        encoded[HEADER_SIZE + 36] ^= 0x01;

        let result = decode_segments(&encoded);
        assert!(matches!(result, Err(DecodeError::ChecksumMismatch { .. })));
    }

    #[test]
    fn test_checksum_detects_single_bit_flip() {
        let segments = vec![WireSegment {
            id: 1,
            timestamp_ms: 1000,
            is_timestamp_from_source: 1,
            audio_data: vec![0x00; 1000], // Large enough to test bit flips
            section_id: 1,
            duration_samples: 960,
        }];

        let encoded = encode_segments(&segments);

        // Test single bit flip at various positions in segment data
        for bit_pos in [0, 7, 100, 500, 999] {
            let mut corrupted = encoded.clone();
            let byte_idx = HEADER_SIZE + SEGMENT_HEADER_SIZE + bit_pos;
            if byte_idx < corrupted.len() {
                corrupted[byte_idx] ^= 0x01; // Flip single bit
                let result = decode_segments(&corrupted);
                assert!(
                    matches!(result, Err(DecodeError::ChecksumMismatch { .. })),
                    "Failed to detect bit flip at position {}",
                    bit_pos
                );
            }
        }
    }

    #[test]
    fn test_checksum_detects_corruption_in_second_segment() {
        let segments = vec![
            WireSegment {
                id: 1,
                timestamp_ms: 1000,
                is_timestamp_from_source: 1,
                audio_data: vec![0xAA; 100],
                section_id: 1,
                duration_samples: 960,
            },
            WireSegment {
                id: 2,
                timestamp_ms: 2000,
                is_timestamp_from_source: 0,
                audio_data: vec![0xBB; 100],
                section_id: 1,
                duration_samples: 960,
            },
        ];

        let mut encoded = encode_segments(&segments);
        // Calculate offset to second segment's audio data
        let second_segment_audio_offset = HEADER_SIZE + SEGMENT_HEADER_SIZE + 100 + SEGMENT_HEADER_SIZE;
        encoded[second_segment_audio_offset] ^= 0xFF;

        let result = decode_segments(&encoded);
        assert!(matches!(result, Err(DecodeError::ChecksumMismatch { .. })));
    }

    #[test]
    fn test_checksum_error_contains_expected_and_computed() {
        let segments = vec![WireSegment {
            id: 1,
            timestamp_ms: 1000,
            is_timestamp_from_source: 1,
            audio_data: vec![0xAA; 100],
            section_id: 1,
            duration_samples: 960,
        }];

        let mut encoded = encode_segments(&segments);
        let original_crc = u32::from_le_bytes([encoded[12], encoded[13], encoded[14], encoded[15]]);

        // Corrupt data
        encoded[HEADER_SIZE + SEGMENT_HEADER_SIZE] ^= 0xFF;

        let result = decode_segments(&encoded);
        match result {
            Err(DecodeError::ChecksumMismatch { expected, computed }) => {
                assert_eq!(expected, original_crc, "Expected CRC should match original");
                assert_ne!(computed, expected, "Computed CRC should differ from expected");
            }
            _ => panic!("Expected ChecksumMismatch error"),
        }
    }

    #[test]
    fn test_truncated_segment_data() {
        let segments = vec![WireSegment {
            id: 1,
            timestamp_ms: 1000,
            is_timestamp_from_source: 1,
            audio_data: vec![0xAA; 100],
            section_id: 1,
            duration_samples: 960,
        }];

        let encoded = encode_segments(&segments);
        // Truncate to cut off some audio data, but keep valid CRC by recalculating
        let truncated_len = HEADER_SIZE + SEGMENT_HEADER_SIZE + 50; // Only 50 of 100 audio bytes
        let mut truncated = encoded[..truncated_len].to_vec();
        // Recalculate CRC for truncated data (so we test length validation, not CRC)
        let crc = crc32fast::hash(&truncated[HEADER_SIZE..]);
        truncated[12..16].copy_from_slice(&crc.to_le_bytes());

        let result = decode_segments(&truncated);
        assert!(matches!(
            result,
            Err(DecodeError::InvalidAudioDataLen { .. })
        ));
    }

    #[test]
    fn test_valid_empty_segments_has_correct_crc() {
        let segments: Vec<WireSegment> = vec![];
        let encoded = encode_segments(&segments);

        // Verify header structure
        assert_eq!(encoded.len(), HEADER_SIZE);
        assert_eq!(
            u32::from_le_bytes([encoded[0], encoded[1], encoded[2], encoded[3]]),
            MAGIC
        );
        assert_eq!(
            u32::from_le_bytes([encoded[4], encoded[5], encoded[6], encoded[7]]),
            VERSION
        );
        assert_eq!(
            u32::from_le_bytes([encoded[8], encoded[9], encoded[10], encoded[11]]),
            0
        ); // count = 0

        // CRC of empty data should be 0
        let crc = u32::from_le_bytes([encoded[12], encoded[13], encoded[14], encoded[15]]);
        assert_eq!(crc, crc32fast::hash(&[]));

        // Should decode successfully
        let decoded = decode_segments(&encoded).unwrap();
        assert_eq!(decoded.len(), 0);
    }
}
