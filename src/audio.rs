/// Create Opus identification header
pub fn create_opus_id_header(channels: u8, sample_rate: u32) -> Vec<u8> {
    let mut header = Vec::with_capacity(19);
    header.extend_from_slice(b"OpusHead");
    header.push(1); // Version
    header.push(channels); // Channel count
    header.extend_from_slice(&0u16.to_le_bytes()); // Pre-skip
    header.extend_from_slice(&sample_rate.to_le_bytes()); // Input sample rate
    header.extend_from_slice(&0i16.to_le_bytes()); // Output gain
    header.push(0); // Channel mapping family
    header
}

pub fn create_opus_comment_header_with_duration(duration_secs: Option<f64>) -> Vec<u8> {
    let mut header = Vec::new();
    header.extend_from_slice(b"OpusTags");

    let vendor = b"save_audio_stream";
    header.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    header.extend_from_slice(vendor);

    match duration_secs {
        Some(dur) => {
            let duration_comment = format!("DURATION={:.3}", dur);
            header.extend_from_slice(&1u32.to_le_bytes());
            header.extend_from_slice(&(duration_comment.len() as u32).to_le_bytes());
            header.extend_from_slice(duration_comment.as_bytes());
        }
        None => {
            header.extend_from_slice(&0u32.to_le_bytes());
        }
    }

    header
}

/// Resample audio from source sample rate to target rate
pub fn resample(samples: &[i16], src_rate: u32, target_rate: u32) -> Vec<i16> {
    if src_rate == target_rate {
        return samples.to_vec();
    }

    let ratio = target_rate as f64 / src_rate as f64;
    let new_len = (samples.len() as f64 * ratio) as usize;
    let mut resampled = Vec::with_capacity(new_len);

    for i in 0..new_len {
        let src_pos = i as f64 / ratio;
        let src_idx = src_pos as usize;
        let frac = src_pos - src_idx as f64;

        let sample = if src_idx + 1 < samples.len() {
            let s1 = samples[src_idx] as f64;
            let s2 = samples[src_idx + 1] as f64;
            (s1 + frac * (s2 - s1)) as i16
        } else if src_idx < samples.len() {
            samples[src_idx]
        } else {
            0
        };

        resampled.push(sample);
    }

    resampled
}
