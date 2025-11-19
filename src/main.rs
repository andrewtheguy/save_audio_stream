use chrono::{DateTime, Utc};
use clap::Parser;
use crossbeam_channel::{bounded, Receiver, Sender};
use hound::{WavSpec, WavWriter};
use reqwest::blocking::Client;
use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

#[derive(Parser, Debug)]
#[command(author, version, about = "Download Shoutcast stream and save as WAV")]
struct Args {
    /// URL of the Shoutcast/Icecast stream
    #[arg(short, long)]
    url: String,

    /// Duration in seconds to record
    #[arg(short, long, default_value = "30")]
    duration: u64,
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

    let output_filename = format!("recording_{}.wav", timestamp);

    println!("Content-Type: {}", content_type);
    println!("Output file: {}", output_filename);

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

    // Spawn download thread
    let duration = args.duration;
    let download_handle = thread::spawn(move || {
        let start_time = Instant::now();
        let duration_limit = Duration::from_secs(duration);
        let mut reader = response;
        let mut chunk = [0u8; 8192];
        let mut bytes_downloaded = 0u64;

        println!("Downloading audio data...");

        while start_time.elapsed() < duration_limit {
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
                        eprintln!("Receiver dropped, stopping download");
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
    let sample_rate = codec_params.sample_rate.ok_or("Unknown sample rate")?;
    let channels = codec_params
        .channels
        .ok_or("Unknown channel count")?
        .count() as u16;

    println!("Audio format: {} Hz, {} channels", sample_rate, channels);

    // Create WAV writer
    let wav_spec = WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut wav_writer = WavWriter::create(&output_filename, wav_spec)?;

    // Decode and write samples in real-time
    println!("Decoding and writing in real-time...");
    let mut total_samples = 0usize;
    let mut packets_decoded = 0usize;

    loop {
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

                        // Write samples directly to WAV
                        for sample in sample_buf.samples() {
                            wav_writer.write_sample(*sample)?;
                        }

                        total_samples += sample_buf.samples().len();
                        packets_decoded += 1;

                        // Progress update every 100 packets
                        if packets_decoded % 100 == 0 {
                            let duration_secs =
                                total_samples as f64 / (sample_rate as f64 * channels as f64);
                            print!("\rDecoded {:.1}s of audio...", duration_secs);
                            std::io::Write::flush(&mut std::io::stdout())?;
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
                break;
            }
            Err(e) => {
                eprintln!("\nFormat error: {}", e);
                break;
            }
        }
    }

    println!(); // New line after progress

    // Wait for download thread to finish
    let bytes_downloaded = download_handle.join().expect("Download thread panicked");

    if total_samples == 0 {
        return Err("No audio samples decoded".into());
    }

    // Finalize WAV file
    wav_writer.finalize()?;

    let duration_secs = total_samples as f64 / (sample_rate as f64 * channels as f64);
    println!("Decoded {} samples from {} packets", total_samples, packets_decoded);
    println!(
        "Successfully saved {} ({:.1} seconds of audio, {} bytes downloaded)",
        output_filename, duration_secs, bytes_downloaded
    );

    Ok(())
}
