use chrono::{DateTime, Utc};
use clap::{Parser, ValueEnum};
use crossbeam_channel::{bounded, Receiver, Sender};
use fdk_aac::enc::{
    AudioObjectType, BitRate as AacBitRate, ChannelMode, Encoder as AacEncoder, EncoderParams,
    Transport,
};
use ogg::writing::PacketWriter;
use opus::{Application, Bitrate as OpusBitrate, Channels, Encoder as OpusEncoder};
use reqwest::blocking::Client;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Instant;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    /// AAC-LC format (16kHz mono, 32kbps)
    Aac,
    /// Opus format (48kHz mono, 16kbps)
    Opus,
}

#[derive(Parser, Debug)]
#[command(author, version, about = "Download Shoutcast stream and save as AAC or Opus")]
struct Args {
    /// URL of the Shoutcast/Icecast stream
    #[arg(short, long)]
    url: String,

    /// Duration in seconds to record
    #[arg(short, long, default_value = "30")]
    duration: u64,

    /// Output format
    #[arg(short, long, value_enum, default_value = "aac")]
    format: OutputFormat,

    /// Bitrate in kbps (default: 32 for AAC, 16 for Opus)
    #[arg(short, long)]
    bitrate: Option<u32>,

    /// Name prefix for output file (default: recording)
    #[arg(short, long, default_value = "recording")]
    name: String,
}

/// A streaming media source that reads from a channel
struct StreamingSource {
    receiver: Receiver<Vec<u8>>,
    buffer: Vec<u8>,
    position: usize,
    total_bytes: Arc<AtomicU64>,
    is_finished: bool,
}

impl StreamingSource {
    fn new(receiver: Receiver<Vec<u8>>, total_bytes: Arc<AtomicU64>) -> Self {
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
                Err(_) => {
                    // Channel closed
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
    let vendor = b"save_audio_stream";
    header.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    header.extend_from_slice(vendor);
    header.extend_from_slice(&0u32.to_le_bytes()); // No user comments
    header
}

/// Resample audio from source sample rate to target rate
fn resample(samples: &[i16], src_rate: u32, target_rate: u32) -> Vec<i16> {
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    println!("Connecting to: {}", args.url);
    println!("Recording duration: {} seconds", args.duration);

    // Create HTTP client and make request
    let client = Client::builder()
        .timeout(None) // No timeout for streaming
        .build()?;

    let response = client.get(&args.url).send()?;

    if !response.status().is_success() {
        return Err(format!("HTTP error: {}", response.status()).into());
    }

    // Extract headers
    let content_type = response
        .headers()
        .get("content-type")
        .ok_or("Missing Content-Type header")?
        .to_str()
        .map_err(|_| "Invalid Content-Type header encoding")?
        .to_string();

    let date_header = response
        .headers()
        .get("date")
        .ok_or("Missing Date header")?
        .to_str()
        .map_err(|_| "Invalid Date header encoding")?;

    // Parse date for filename
    let timestamp = {
        let system_time = httpdate::parse_http_date(date_header)
            .map_err(|_| format!("Failed to parse Date header: {}", date_header))?;
        let datetime: DateTime<Utc> = system_time.into();
        datetime.format("%Y%m%d_%H%M%S").to_string()
    };

    let output_filename = match args.format {
        OutputFormat::Aac => format!("{}_{}.aac", args.name, timestamp),
        OutputFormat::Opus => format!("{}_{}.opus", args.name, timestamp),
    };

    println!("Content-Type: {}", content_type);
    println!("Output file: {}", output_filename);
    println!("Output format: {:?}", args.format);

    // Determine codec from content type
    let codec_hint = match content_type.as_str() {
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/aac" | "audio/aacp" | "audio/x-aac" => "aac",
        _ => {
            return Err(format!(
                "Unsupported Content-Type: '{}'. Supported types: audio/mpeg, audio/mp3, audio/aac, audio/aacp, audio/x-aac",
                content_type
            ).into());
        }
    };

    println!("Detected codec: {}", codec_hint);

    // Create channel for streaming data
    let (tx, rx): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = bounded(100);
    let total_bytes = Arc::new(AtomicU64::new(0));
    let total_bytes_clone = Arc::clone(&total_bytes);
    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_flag_clone = Arc::clone(&stop_flag);

    // Spawn download thread
    let download_handle = thread::spawn(move || {
        let start_time = Instant::now();
        let mut reader = response;
        let mut chunk = [0u8; 8192];
        let mut bytes_downloaded = 0u64;

        println!("Downloading audio data...");

        while !stop_flag_clone.load(Ordering::Relaxed) {
            match reader.read(&mut chunk) {
                Ok(0) => {
                    println!("Stream ended");
                    break;
                }
                Ok(n) => {
                    bytes_downloaded += n as u64;
                    total_bytes_clone.store(bytes_downloaded, Ordering::Relaxed);

                    // Send chunk through channel
                    if tx.send(chunk[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    eprintln!("Read error: {}", e);
                    break;
                }
            }
        }

        println!(
            "Download complete: {} bytes in {:.1} seconds",
            bytes_downloaded,
            start_time.elapsed().as_secs_f64()
        );

        // Signal end of stream
        let _ = tx.send(Vec::new());
        bytes_downloaded
    });

    // Create streaming source for decoder
    let streaming_source = StreamingSource::new(rx, total_bytes);
    let mss = MediaSourceStream::new(Box::new(streaming_source), Default::default());

    // Create a hint to help the format registry guess the format
    let mut hint = Hint::new();
    hint.with_extension(codec_hint);

    // Use the default options for format reader and metadata
    let format_opts = FormatOptions::default();
    let metadata_opts = MetadataOptions::default();

    // Probe the media source
    println!("Probing audio format...");
    let probed =
        symphonia::default::get_probe().format(&hint, mss, &format_opts, &metadata_opts)?;

    let mut format = probed.format;

    // Find the first audio track
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
        .ok_or("No audio track found")?;

    let track_id = track.id;
    let codec_params = track.codec_params.clone();

    // Create a decoder for the track
    let decoder_opts = DecoderOptions::default();
    let mut decoder = symphonia::default::get_codecs().make(&codec_params, &decoder_opts)?;

    // Get audio parameters
    let src_sample_rate = codec_params.sample_rate.ok_or("Unknown sample rate")?;
    let src_channels = codec_params
        .channels
        .ok_or("Unknown channel count")?
        .count() as u16;

    println!(
        "Source audio format: {} Hz, {} channels",
        src_sample_rate, src_channels
    );

    // Format-specific setup
    let (output_sample_rate, frame_size, default_bitrate) = match args.format {
        OutputFormat::Aac => (16000u32, 1024usize, 32u32),
        OutputFormat::Opus => (48000u32, 960usize, 16u32),
    };
    let bitrate_kbps = args.bitrate.unwrap_or(default_bitrate);
    let bitrate = bitrate_kbps as i32 * 1000;

    println!(
        "Output: {} Hz, mono, {} kbps {:?}",
        output_sample_rate,
        bitrate_kbps,
        args.format
    );

    // Create encoders based on format
    let mut aac_encoder = None;
    let mut opus_encoder = None;
    let mut ogg_writer = None;
    let mut output_file = None;

    match args.format {
        OutputFormat::Aac => {
            let params = EncoderParams {
                bit_rate: AacBitRate::Cbr(bitrate as u32),
                sample_rate: 16000,
                channels: ChannelMode::Mono,
                transport: Transport::Adts,
                audio_object_type: AudioObjectType::Mpeg4LowComplexity,
            };
            aac_encoder = Some(
                AacEncoder::new(params)
                    .map_err(|e| format!("Failed to create AAC encoder: {:?}", e))?,
            );
            output_file = Some(File::create(&output_filename)?);
        }
        OutputFormat::Opus => {
            let mut encoder = OpusEncoder::new(48000, Channels::Mono, Application::Voip)
                .map_err(|e| format!("Failed to create Opus encoder: {}", e))?;
            encoder
                .set_bitrate(OpusBitrate::Bits(bitrate))
                .map_err(|e| format!("Failed to set bitrate: {}", e))?;
            opus_encoder = Some(encoder);

            let file = File::create(&output_filename)?;
            let mut writer = PacketWriter::new(file);

            // Write Opus headers
            let serial = 1;
            let id_header = create_opus_id_header(1, src_sample_rate);
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

            ogg_writer = Some(writer);
        }
    }

    // Decode and encode in real-time
    println!("Decoding and encoding to {:?}...", args.format);
    let mut total_input_samples = 0usize;
    let mut packets_decoded = 0usize;
    let mut total_output_samples: u64 = 0;

    // Buffer for collecting mono samples before encoding
    let mut mono_buffer: Vec<i16> = Vec::new();
    let mut encode_output = vec![0u8; 8192]; // Buffer for encoded output

    // Target duration in samples at output rate
    let target_samples = args.duration * output_sample_rate as u64;

    loop {
        // Check if we've reached target duration
        if total_output_samples >= target_samples {
            stop_flag.store(true, Ordering::Relaxed);
            break;
        }

        match format.next_packet() {
            Ok(packet) => {
                if packet.track_id() != track_id {
                    continue;
                }

                match decoder.decode(&packet) {
                    Ok(decoded) => {
                        // Get the audio buffer spec
                        let spec = *decoded.spec();
                        let duration = decoded.capacity() as u64;

                        // Create a sample buffer to hold the decoded samples
                        let mut sample_buf = SampleBuffer::<i16>::new(duration, spec);
                        sample_buf.copy_interleaved_ref(decoded);

                        let samples = sample_buf.samples();
                        total_input_samples += samples.len();

                        // Convert to mono
                        let mono_samples: Vec<i16> = if src_channels == 1 {
                            samples.to_vec()
                        } else {
                            // Average channels to mono
                            samples
                                .chunks(src_channels as usize)
                                .map(|chunk| {
                                    let sum: i32 = chunk.iter().map(|&s| s as i32).sum();
                                    (sum / chunk.len() as i32) as i16
                                })
                                .collect()
                        };

                        // Resample to output sample rate
                        let resampled = resample(&mono_samples, src_sample_rate, output_sample_rate);
                        mono_buffer.extend_from_slice(&resampled);

                        // Encode complete frames
                        while mono_buffer.len() >= frame_size {
                            let frame: Vec<i16> = mono_buffer.drain(..frame_size).collect();

                            match args.format {
                                OutputFormat::Aac => {
                                    if let Some(ref encoder) = aac_encoder {
                                        match encoder.encode(&frame, &mut encode_output) {
                                            Ok(info) => {
                                                total_output_samples += frame_size as u64;
                                                if let Some(ref mut file) = output_file {
                                                    file.write_all(&encode_output[..info.output_size])?;
                                                }
                                            }
                                            Err(e) => {
                                                eprintln!("AAC encode error: {:?}", e);
                                            }
                                        }
                                    }
                                }
                                OutputFormat::Opus => {
                                    if let Some(ref mut encoder) = opus_encoder {
                                        match encoder.encode(&frame, &mut encode_output) {
                                            Ok(len) => {
                                                total_output_samples += frame_size as u64;
                                                if let Some(ref mut writer) = ogg_writer {
                                                    writer.write_packet(
                                                        encode_output[..len].to_vec(),
                                                        1,
                                                        ogg::writing::PacketWriteEndInfo::NormalPacket,
                                                        total_output_samples,
                                                    )?;
                                                }
                                            }
                                            Err(e) => {
                                                eprintln!("Opus encode error: {:?}", e);
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        packets_decoded += 1;

                        // Progress update every 100 packets
                        if packets_decoded % 100 == 0 {
                            let duration_secs = total_input_samples as f64
                                / (src_sample_rate as f64 * src_channels as f64);
                            print!("\rDecoded {:.1}s of audio...", duration_secs);
                            std::io::stdout().flush()?;
                        }
                    }
                    Err(symphonia::core::errors::Error::DecodeError(e)) => {
                        eprintln!("\nDecode error: {}", e);
                        continue;
                    }
                    Err(e) => {
                        eprintln!("\nFatal decode error: {}", e);
                        break;
                    }
                }
            }
            Err(symphonia::core::errors::Error::IoError(e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                // End of stream
                stop_flag.store(true, Ordering::Relaxed);
                break;
            }
            Err(e) => {
                eprintln!("\nFormat error: {}", e);
                stop_flag.store(true, Ordering::Relaxed);
                break;
            }
        }
    }

    // Encode any remaining samples (pad with silence if needed)
    if !mono_buffer.is_empty() {
        mono_buffer.resize(frame_size, 0);
        match args.format {
            OutputFormat::Aac => {
                if let Some(ref encoder) = aac_encoder {
                    if let Ok(info) = encoder.encode(&mono_buffer, &mut encode_output) {
                        total_output_samples += frame_size as u64;
                        if let Some(ref mut file) = output_file {
                            file.write_all(&encode_output[..info.output_size])?;
                        }
                    }
                }
            }
            OutputFormat::Opus => {
                if let Some(ref mut encoder) = opus_encoder {
                    if let Ok(len) = encoder.encode(&mono_buffer, &mut encode_output) {
                        total_output_samples += frame_size as u64;
                        if let Some(ref mut writer) = ogg_writer {
                            writer.write_packet(
                                encode_output[..len].to_vec(),
                                1,
                                ogg::writing::PacketWriteEndInfo::EndStream,
                                total_output_samples,
                            )?;
                        }
                    }
                }
            }
        }
    }

    println!(); // New line after progress

    // Wait for download thread to finish
    let bytes_downloaded = download_handle.join().expect("Download thread panicked");

    if total_input_samples == 0 {
        return Err("No audio samples decoded".into());
    }

    let duration_secs =
        total_input_samples as f64 / (src_sample_rate as f64 * src_channels as f64);
    println!(
        "Decoded {} samples from {} packets",
        total_input_samples, packets_decoded
    );
    println!(
        "Successfully saved {} ({:.1} seconds of audio, {} bytes downloaded)",
        output_filename, duration_secs, bytes_downloaded
    );

    Ok(())
}
