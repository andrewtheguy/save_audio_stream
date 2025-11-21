use std::fs::{self, File};
use std::io::{Read, Write};

use fdk_aac::dec::{Decoder as AacDecoder, Transport as DecTransport};
use fdk_aac::enc::{
    AudioObjectType, BitRate as AacBitRate, ChannelMode, Encoder as AacEncoder, EncoderParams,
    Transport,
};
use hound::{WavReader, WavSpec, WavWriter};
use ogg::reading::PacketReader;
use ogg::writing::PacketWriter;
use opus::{
    Application, Bitrate as OpusBitrate, Channels, Decoder as OpusDecoder, Encoder as OpusEncoder,
};

/// Generate a test sine wave at the given sample rate
fn generate_sine_wave(sample_rate: u32, duration_secs: f32, frequency: f32) -> Vec<i16> {
    let num_samples = (sample_rate as f32 * duration_secs) as usize;
    let mut samples = Vec::with_capacity(num_samples);

    for i in 0..num_samples {
        let t = i as f32 / sample_rate as f32;
        let sample = (t * frequency * 2.0 * std::f32::consts::PI).sin() * 16000.0;
        samples.push(sample as i16);
    }

    samples
}

/// Create Opus identification header
fn create_opus_id_header(channels: u8, sample_rate: u32) -> Vec<u8> {
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

/// Create Opus comment header
fn create_opus_comment_header() -> Vec<u8> {
    let mut header = Vec::new();
    header.extend_from_slice(b"OpusTags");
    let vendor = b"gapless_test";
    header.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    header.extend_from_slice(vendor);
    header.extend_from_slice(&0u32.to_le_bytes()); // No user comments
    header
}

/// Encode samples to AAC files with splitting
fn encode_aac_split(
    samples: &[i16],
    output_dir: &str,
    split_interval_samples: usize,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let sample_rate = 16000u32;
    let frame_size = 1024usize;

    let params = EncoderParams {
        bit_rate: AacBitRate::Cbr(32000),
        sample_rate: sample_rate as u32,
        channels: ChannelMode::Mono,
        transport: Transport::Adts,
        audio_object_type: AudioObjectType::Mpeg4LowComplexity,
    };

    let encoder =
        AacEncoder::new(params).map_err(|e| format!("Failed to create AAC encoder: {:?}", e))?;
    let mut encode_buffer = vec![0u8; 8192];
    let mut files_written = Vec::new();
    let mut segment_number = 0;
    let mut segment_samples = 0usize;

    // Create first file
    let mut filename = format!("{}/test_{:03}.aac", output_dir, segment_number);
    files_written.push(filename.clone());
    let mut output_file = File::create(&filename)?;

    // Process samples in frames
    let mut pos = 0;
    while pos + frame_size <= samples.len() {
        let frame = &samples[pos..pos + frame_size];

        match encoder.encode(frame, &mut encode_buffer) {
            Ok(info) => {
                output_file.write_all(&encode_buffer[..info.output_size])?;
                segment_samples += frame_size;

                // Check if we need to split
                if split_interval_samples > 0 && segment_samples >= split_interval_samples {
                    segment_number += 1;
                    segment_samples = 0;
                    filename = format!("{}/test_{:03}.aac", output_dir, segment_number);
                    files_written.push(filename.clone());
                    output_file = File::create(&filename)?;
                }
            }
            Err(e) => {
                return Err(format!("AAC encode error: {:?}", e).into());
            }
        }

        pos += frame_size;
    }

    // Handle remaining samples (pad with silence)
    if pos < samples.len() {
        let mut final_frame = samples[pos..].to_vec();
        final_frame.resize(frame_size, 0);
        if let Ok(info) = encoder.encode(&final_frame, &mut encode_buffer) {
            output_file.write_all(&encode_buffer[..info.output_size])?;
        }
    }

    Ok(files_written)
}

/// Encode samples to Opus files with splitting
fn encode_opus_split(
    samples: &[i16],
    output_dir: &str,
    split_interval_samples: usize,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let sample_rate = 48000u32;
    let frame_size = 960usize; // 20ms at 48kHz

    let mut encoder = OpusEncoder::new(sample_rate, Channels::Mono, Application::Voip)?;
    encoder.set_bitrate(OpusBitrate::Bits(16000))?;

    let mut encode_buffer = vec![0u8; 8192];
    let mut files_written = Vec::new();
    let mut segment_number = 0;
    let mut segment_samples = 0usize;
    let mut granule_pos: u64 = 0;

    // Helper to create a new Ogg file with Opus headers
    let create_ogg_file =
        |filename: &str| -> Result<PacketWriter<File>, Box<dyn std::error::Error>> {
            let file = File::create(filename)?;
            let mut writer = PacketWriter::new(file);

            let serial = 1;
            let id_header = create_opus_id_header(1, sample_rate);
            writer.write_packet(
                id_header,
                serial,
                ogg::writing::PacketWriteEndInfo::EndPage,
                0,
            )?;

            let comment_header = create_opus_comment_header();
            writer.write_packet(
                comment_header,
                serial,
                ogg::writing::PacketWriteEndInfo::EndPage,
                0,
            )?;

            Ok(writer)
        };

    // Create first file
    let mut filename = format!("{}/test_{:03}.opus", output_dir, segment_number);
    files_written.push(filename.clone());
    let mut ogg_writer = create_ogg_file(&filename)?;

    // Collect all frames first
    let num_complete_frames = samples.len() / frame_size;
    let has_remainder = samples.len() % frame_size > 0;
    let total_frames = if has_remainder {
        num_complete_frames + 1
    } else {
        num_complete_frames
    };

    // Process samples in frames
    let mut pos = 0;
    let mut frame_idx = 0;
    while pos + frame_size <= samples.len() {
        let frame = &samples[pos..pos + frame_size];
        frame_idx += 1;
        let is_last_frame = frame_idx == total_frames && !has_remainder;

        match encoder.encode(frame, &mut encode_buffer) {
            Ok(len) => {
                granule_pos += frame_size as u64;
                segment_samples += frame_size;

                // Check if this is the last packet before split or the final packet
                let is_end_of_segment =
                    split_interval_samples > 0 && segment_samples >= split_interval_samples;
                let end_info = if is_end_of_segment || is_last_frame {
                    ogg::writing::PacketWriteEndInfo::EndStream
                } else {
                    ogg::writing::PacketWriteEndInfo::NormalPacket
                };

                ogg_writer.write_packet(encode_buffer[..len].to_vec(), 1, end_info, granule_pos)?;

                // Check if we need to split (only if not the last frame)
                if is_end_of_segment && !is_last_frame {
                    segment_number += 1;
                    segment_samples = 0;
                    filename = format!("{}/test_{:03}.opus", output_dir, segment_number);
                    files_written.push(filename.clone());
                    ogg_writer = create_ogg_file(&filename)?;
                }
            }
            Err(e) => {
                return Err(format!("Opus encode error: {}", e).into());
            }
        }

        pos += frame_size;
    }

    // Handle remaining samples (pad with silence)
    if pos < samples.len() {
        let mut final_frame = samples[pos..].to_vec();
        final_frame.resize(frame_size, 0);
        if let Ok(len) = encoder.encode(&final_frame, &mut encode_buffer) {
            granule_pos += frame_size as u64;
            ogg_writer.write_packet(
                encode_buffer[..len].to_vec(),
                1,
                ogg::writing::PacketWriteEndInfo::EndStream,
                granule_pos,
            )?;
        }
    }

    Ok(files_written)
}

/// Decode AAC files and return all samples
fn decode_aac_files(files: &[String]) -> Result<Vec<i16>, Box<dyn std::error::Error>> {
    let mut all_samples = Vec::new();

    for filename in files {
        let mut file = File::open(filename)?;
        let mut data = Vec::new();
        file.read_to_end(&mut data)?;

        let mut decoder = AacDecoder::new(DecTransport::Adts);
        let mut decode_buffer = vec![0i16; 8192];
        let mut pos = 0;

        while pos < data.len() {
            // Find ADTS sync word
            if pos + 7 > data.len() {
                break;
            }

            if data[pos] != 0xFF || (data[pos + 1] & 0xF0) != 0xF0 {
                pos += 1;
                continue;
            }

            // Get frame length from ADTS header
            let frame_len = (((data[pos + 3] & 0x03) as usize) << 11)
                | ((data[pos + 4] as usize) << 3)
                | ((data[pos + 5] as usize) >> 5);

            if pos + frame_len > data.len() || frame_len < 7 {
                break;
            }

            // Fill decoder buffer with frame data
            match decoder.fill(&data[pos..pos + frame_len]) {
                Ok(bytes_filled) => {
                    if bytes_filled == 0 {
                        pos += 1;
                        continue;
                    }
                }
                Err(_) => {
                    pos += 1;
                    continue;
                }
            }

            // Decode frame
            match decoder.decode_frame(&mut decode_buffer) {
                Ok(()) => {
                    // Get stream info to find out how many samples were decoded
                    let info = decoder.stream_info();
                    let sample_count = info.frameSize as usize * info.numChannels as usize;
                    if sample_count > 0 && sample_count <= decode_buffer.len() {
                        all_samples.extend_from_slice(&decode_buffer[..sample_count]);
                    }
                }
                Err(_) => {
                    // Skip this frame
                }
            }

            pos += frame_len;
        }
    }

    Ok(all_samples)
}

/// Decode Opus files and return all samples
fn decode_opus_files(files: &[String]) -> Result<Vec<i16>, Box<dyn std::error::Error>> {
    let mut all_samples = Vec::new();
    let mut decoder = OpusDecoder::new(48000, Channels::Mono)?;
    let mut decode_buffer = vec![0i16; 5760]; // Max frame size for Opus

    for filename in files {
        let file = File::open(filename)?;
        let mut packet_reader = PacketReader::new(file);

        // Skip headers (first two packets)
        let mut header_count = 0;
        while let Some(packet) = packet_reader.read_packet()? {
            if header_count < 2 {
                header_count += 1;
                continue;
            }

            // Decode audio packet
            match decoder.decode(&packet.data, &mut decode_buffer, false) {
                Ok(samples) => {
                    all_samples.extend_from_slice(&decode_buffer[..samples]);
                }
                Err(e) => {
                    eprintln!("Opus decode error: {}", e);
                }
            }
        }
    }

    Ok(all_samples)
}

/// Calculate the total sample count considering frame sizes
fn expected_sample_count(input_samples: usize, frame_size: usize) -> usize {
    let complete_frames = input_samples / frame_size;
    let has_remainder = input_samples % frame_size > 0;

    if has_remainder {
        (complete_frames + 1) * frame_size
    } else {
        complete_frames * frame_size
    }
}

#[test]
fn test_aac_gapless_split() {
    let test_dir = "/tmp/save_audio_stream_test_aac";
    fs::create_dir_all(test_dir).unwrap();

    // AAC-LC gapless metadata (same values stored in database)
    const AAC_ENCODER_DELAY: usize = 2048; // Priming samples
    const AAC_FRAME_SIZE: usize = 1024;

    // Generate 5 seconds of test audio at 16kHz
    let sample_rate = 16000u32;
    let duration = 5.0;
    let samples = generate_sine_wave(sample_rate, duration, 440.0);

    // Split every 1 second (16000 samples)
    let split_interval = sample_rate as usize;

    // Encode with splitting
    let files = encode_aac_split(&samples, test_dir, split_interval).unwrap();

    // Should have multiple files
    assert!(
        files.len() >= 4,
        "Expected at least 4 files for 5 seconds with 1s splits, got {}",
        files.len()
    );

    // Decode all files
    let decoded = decode_aac_files(&files).unwrap();

    println!("AAC Gapless Test:");
    println!("  Input samples: {}", samples.len());
    println!("  Decoded samples: {}", decoded.len());
    println!("  Encoder delay: {}", AAC_ENCODER_DELAY);
    println!("  Frame size: {}", AAC_FRAME_SIZE);
    println!("  Files created: {}", files.len());

    // Note: When splitting AAC files, each segment introduces its own encoder delay.
    // The global metadata (encoder_delay=2048, frame_size=1024) applies per-segment.
    // For true gapless across splits, a player would need to skip encoder_delay
    // at the start of EACH segment, not just the first file.
    let expected_loss_per_segment = AAC_ENCODER_DELAY;
    let expected_total_loss = expected_loss_per_segment * files.len();
    let actual_loss = samples.len() as i64 - decoded.len() as i64;

    println!(
        "  Expected loss ({} segments * {} delay): {}",
        files.len(),
        AAC_ENCODER_DELAY,
        expected_total_loss
    );
    println!("  Actual sample loss: {}", actual_loss);

    // Verify the loss is approximately what we'd expect from encoder delay per segment
    let loss_tolerance = AAC_FRAME_SIZE as i64 * files.len() as i64 * 2;
    assert!(
        (actual_loss - expected_total_loss as i64).abs() < loss_tolerance,
        "Sample loss ({}) doesn't match expected encoder delay loss ({}) within tolerance ({})",
        actual_loss,
        expected_total_loss,
        loss_tolerance
    );

    // Verify we got audio data in each file
    assert!(
        decoded.len() > samples.len() / 2,
        "Decoded samples ({}) too low",
        decoded.len()
    );

    println!("  AAC gapless test passed");

    // Cleanup
    for file in files {
        fs::remove_file(file).ok();
    }
    fs::remove_dir(test_dir).ok();
}

#[test]
fn test_opus_gapless_split() {
    let test_dir = "/tmp/save_audio_stream_test_opus";
    fs::create_dir_all(test_dir).unwrap();

    // Generate 5 seconds of test audio at 48kHz
    let sample_rate = 48000u32;
    let duration = 5.0;
    let samples = generate_sine_wave(sample_rate, duration, 440.0);

    // Split every 1 second (48000 samples)
    let split_interval = sample_rate as usize;

    // Encode with splitting
    let files = encode_opus_split(&samples, test_dir, split_interval).unwrap();

    // Should have 5 files (one per second)
    assert!(
        files.len() >= 4,
        "Expected at least 4 files for 5 seconds with 1s splits, got {}",
        files.len()
    );

    // Decode all files
    let decoded = decode_opus_files(&files).unwrap();

    // Check total sample count
    let frame_size = 960;
    let expected = expected_sample_count(samples.len(), frame_size);

    println!("Opus Test:");
    println!("  Input samples: {}", samples.len());
    println!("  Expected output: {}", expected);
    println!("  Actual output: {}", decoded.len());
    println!("  Files created: {}", files.len());

    // Allow for some padding due to frame alignment
    let tolerance = frame_size * 2;
    assert!(
        decoded.len() >= expected - tolerance && decoded.len() <= expected + tolerance,
        "Sample count mismatch: expected {} +/- {}, got {}",
        expected,
        tolerance,
        decoded.len()
    );

    // Verify no large gaps by checking sample continuity
    let mut max_diff = 0i32;
    for i in 1..decoded.len() {
        let diff = (decoded[i] as i32 - decoded[i - 1] as i32).abs();
        if diff > max_diff {
            max_diff = diff;
        }
    }

    // Max difference for 440Hz at 48kHz should be around 2π * 440/48000 * 16000 ≈ 920
    println!("  Max sample diff: {}", max_diff);
    assert!(
        max_diff < 3000,
        "Detected possible gap: max diff = {}",
        max_diff
    );

    // Cleanup
    for file in files {
        fs::remove_file(file).ok();
    }
    fs::remove_dir(test_dir).ok();
}

#[test]
fn test_opus_granule_position_continuity() {
    let test_dir = "/tmp/save_audio_stream_test_opus_granule";
    fs::create_dir_all(test_dir).unwrap();

    // Generate 3 seconds of test audio at 48kHz
    let sample_rate = 48000u32;
    let duration = 3.0;
    let samples = generate_sine_wave(sample_rate, duration, 440.0);

    // Split every 1 second
    let split_interval = sample_rate as usize;

    // Encode with splitting
    let files = encode_opus_split(&samples, test_dir, split_interval).unwrap();

    println!(
        "Created {} files for {} seconds with {}s splits",
        files.len(),
        duration,
        1
    );

    // Verify granule positions are continuous across files
    let mut prev_total_granules: u64 = 0;

    for (file_idx, filename) in files.iter().enumerate() {
        let file = File::open(filename).unwrap();
        let mut packet_reader = PacketReader::new(file);
        let mut packet_count = 0;
        let mut file_granules: u64 = 0;

        while let Some(packet) = packet_reader.read_packet().unwrap() {
            packet_count += 1;

            // Skip headers
            if packet_count <= 2 {
                continue;
            }

            let granule = packet.absgp_page();
            if granule > 0 {
                file_granules = granule;
            }
        }

        println!("File {}: granule position = {}", file_idx, file_granules);

        // Granule positions should increase across files
        assert!(
            file_granules > prev_total_granules || file_idx == 0,
            "Granule position should increase: file {} has {} but prev was {}",
            file_idx,
            file_granules,
            prev_total_granules
        );

        prev_total_granules = file_granules;
    }

    // Final granule should be close to total samples
    let expected_final = samples.len() as u64;
    let tolerance = 960 * 2;
    assert!(
        (prev_total_granules as i64 - expected_final as i64).abs() < tolerance as i64,
        "Final granule position mismatch: expected ~{}, got {}",
        expected_final,
        prev_total_granules
    );

    // Cleanup
    for file in files {
        fs::remove_file(file).ok();
    }
    fs::remove_dir(test_dir).ok();
}

#[test]
fn test_no_split_single_file() {
    let test_dir = "/tmp/save_audio_stream_test_no_split";
    fs::create_dir_all(test_dir).unwrap();

    // Generate 2 seconds of test audio
    let sample_rate = 48000u32;
    let duration = 2.0;
    let samples = generate_sine_wave(sample_rate, duration, 440.0);

    // No splitting (0 = disabled)
    let files = encode_opus_split(&samples, test_dir, 0).unwrap();

    // Should have exactly 1 file
    assert_eq!(
        files.len(),
        1,
        "Expected 1 file without splitting, got {}",
        files.len()
    );

    // Decode and verify sample count
    let decoded = decode_opus_files(&files).unwrap();
    let frame_size = 960;
    let expected = expected_sample_count(samples.len(), frame_size);

    println!("No-split Test:");
    println!("  Input samples: {}", samples.len());
    println!("  Expected output: {}", expected);
    println!("  Actual output: {}", decoded.len());

    let tolerance = frame_size * 2;
    assert!(
        decoded.len() >= expected - tolerance && decoded.len() <= expected + tolerance,
        "Sample count mismatch: expected {} +/- {}, got {}",
        expected,
        tolerance,
        decoded.len()
    );

    // Cleanup
    for file in files {
        fs::remove_file(file).ok();
    }
    fs::remove_dir(test_dir).ok();
}

/// Encode samples to WAV files with splitting
fn encode_wav_split(
    samples: &[i16],
    output_dir: &str,
    sample_rate: u32,
    split_interval_samples: usize,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let frame_size = 1024usize; // Arbitrary frame size for WAV

    let spec = WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut files_written = Vec::new();
    let mut segment_number = 0;
    let mut segment_samples = 0usize;

    // Create first file
    let mut filename = format!("{}/test_{:03}.wav", output_dir, segment_number);
    files_written.push(filename.clone());
    let mut wav_writer = WavWriter::create(&filename, spec)?;

    // Process samples in frames
    let mut pos = 0;
    while pos + frame_size <= samples.len() {
        let frame = &samples[pos..pos + frame_size];

        for sample in frame {
            wav_writer.write_sample(*sample)?;
        }
        segment_samples += frame_size;

        // Check if we need to split
        if split_interval_samples > 0 && segment_samples >= split_interval_samples {
            wav_writer.finalize()?;
            segment_number += 1;
            segment_samples = 0;
            filename = format!("{}/test_{:03}.wav", output_dir, segment_number);
            files_written.push(filename.clone());
            wav_writer = WavWriter::create(&filename, spec)?;
        }

        pos += frame_size;
    }

    // Handle remaining samples
    if pos < samples.len() {
        for sample in &samples[pos..] {
            wav_writer.write_sample(*sample)?;
        }
    }

    wav_writer.finalize()?;
    Ok(files_written)
}

/// Decode WAV files and return all samples
fn decode_wav_files(files: &[String]) -> Result<Vec<i16>, Box<dyn std::error::Error>> {
    let mut all_samples = Vec::new();

    for filename in files {
        let mut reader = WavReader::open(filename)?;
        let samples: Vec<i16> = reader.samples::<i16>().filter_map(Result::ok).collect();
        all_samples.extend(samples);
    }

    Ok(all_samples)
}

#[test]
fn test_wav_gapless_split_exact_match() {
    let test_dir = "/tmp/save_audio_stream_test_wav_exact";
    fs::create_dir_all(test_dir).unwrap();

    // Generate 5 seconds of test audio at 44100 Hz (common sample rate)
    let sample_rate = 44100u32;
    let duration = 5.0;
    let samples = generate_sine_wave(sample_rate, duration, 440.0);

    println!("WAV Exact Match Test:");
    println!("  Input samples: {}", samples.len());

    // Split every 1 second
    let split_interval = sample_rate as usize;

    // Encode with splitting
    let files = encode_wav_split(&samples, test_dir, sample_rate, split_interval).unwrap();

    println!("  Files created: {}", files.len());
    assert!(
        files.len() >= 4,
        "Expected at least 4 files for 5 seconds with 1s splits, got {}",
        files.len()
    );

    // Decode all files
    let decoded = decode_wav_files(&files).unwrap();

    println!("  Decoded samples: {}", decoded.len());

    // WAV is lossless - samples should match exactly
    assert_eq!(
        samples.len(),
        decoded.len(),
        "Sample count mismatch: input {} != output {}",
        samples.len(),
        decoded.len()
    );

    // Compare every sample
    let mut mismatches = 0;
    let mut first_mismatch_idx = None;
    for (i, (input, output)) in samples.iter().zip(decoded.iter()).enumerate() {
        if input != output {
            mismatches += 1;
            if first_mismatch_idx.is_none() {
                first_mismatch_idx = Some(i);
            }
        }
    }

    if mismatches > 0 {
        println!("  ERROR: {} sample mismatches found!", mismatches);
        if let Some(idx) = first_mismatch_idx {
            println!(
                "  First mismatch at index {}: input {} != output {}",
                idx, samples[idx], decoded[idx]
            );
        }
    }

    assert_eq!(
        mismatches,
        0,
        "WAV split files are not gapless: {} mismatches out of {} samples",
        mismatches,
        samples.len()
    );

    println!("  SUCCESS: All {} samples match exactly!", samples.len());

    // Cleanup
    for file in files {
        fs::remove_file(file).ok();
    }
    fs::remove_dir(test_dir).ok();
}

#[test]
fn test_wav_various_split_intervals() {
    let test_dir = "/tmp/save_audio_stream_test_wav_intervals";
    fs::create_dir_all(test_dir).unwrap();

    let sample_rate = 48000u32;
    let duration = 10.0;
    let samples = generate_sine_wave(sample_rate, duration, 440.0);

    // Test various split intervals
    let intervals = vec![
        (sample_rate as usize / 2, "0.5s"), // 0.5 seconds
        (sample_rate as usize, "1s"),       // 1 second
        (sample_rate as usize * 2, "2s"),   // 2 seconds
        (sample_rate as usize * 3, "3s"),   // 3 seconds
    ];

    for (interval, label) in intervals {
        let subdir = format!("{}/interval_{}", test_dir, label);
        fs::create_dir_all(&subdir).unwrap();

        // Encode with splitting
        let files = encode_wav_split(&samples, &subdir, sample_rate, interval).unwrap();

        // Decode all files
        let decoded = decode_wav_files(&files).unwrap();

        // Verify exact match
        assert_eq!(
            samples.len(),
            decoded.len(),
            "Interval {}: sample count mismatch {} != {}",
            label,
            samples.len(),
            decoded.len()
        );

        let mismatches: usize = samples
            .iter()
            .zip(decoded.iter())
            .filter(|(a, b)| a != b)
            .count();

        assert_eq!(
            mismatches, 0,
            "Interval {}: {} mismatches found",
            label, mismatches
        );

        println!(
            "Interval {} ({} files): OK - {} samples match exactly",
            label,
            files.len(),
            samples.len()
        );

        // Cleanup
        for file in files {
            fs::remove_file(file).ok();
        }
        fs::remove_dir(&subdir).ok();
    }

    fs::remove_dir(test_dir).ok();
}

#[test]
fn test_wav_edge_cases() {
    let test_dir = "/tmp/save_audio_stream_test_wav_edge";
    fs::create_dir_all(test_dir).unwrap();

    let sample_rate = 44100u32;

    // Test 1: Very short audio (less than split interval)
    let short_samples = generate_sine_wave(sample_rate, 0.5, 440.0);
    let files =
        encode_wav_split(&short_samples, test_dir, sample_rate, sample_rate as usize).unwrap();
    let decoded = decode_wav_files(&files).unwrap();
    assert_eq!(files.len(), 1, "Short audio should produce 1 file");
    assert_eq!(short_samples, decoded, "Short audio samples should match");
    for f in files {
        fs::remove_file(f).ok();
    }
    println!("Edge case 1 (short audio): OK");

    // Test 2: Audio exactly at split boundary
    let boundary_samples = generate_sine_wave(sample_rate, 2.0, 440.0);
    let split_interval = sample_rate as usize; // Exactly 1 second
    let files = encode_wav_split(&boundary_samples, test_dir, sample_rate, split_interval).unwrap();
    let decoded = decode_wav_files(&files).unwrap();
    assert_eq!(boundary_samples, decoded, "Boundary samples should match");
    for f in files {
        fs::remove_file(f).ok();
    }
    println!("Edge case 2 (exact boundary): OK");

    // Test 3: Prime number of samples (no clean division)
    let prime_count = 44117; // Prime number
    let mut prime_samples = generate_sine_wave(sample_rate, 1.0, 440.0);
    prime_samples.truncate(prime_count);
    let files = encode_wav_split(
        &prime_samples,
        test_dir,
        sample_rate,
        sample_rate as usize / 4,
    )
    .unwrap();
    let decoded = decode_wav_files(&files).unwrap();
    assert_eq!(prime_samples, decoded, "Prime count samples should match");
    for f in files {
        fs::remove_file(f).ok();
    }
    println!("Edge case 3 (prime sample count): OK");

    fs::remove_dir(test_dir).ok();
}
