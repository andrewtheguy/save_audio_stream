use clap::Parser;
use chrono::{DateTime, Utc};
use hound::{WavSpec, WavWriter};
use reqwest::blocking::Client;
use std::io::{Cursor, Read};
use std::time::{Duration, Instant};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
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
        .and_then(|v| v.to_str().ok())
        .unwrap_or("audio/mpeg")
        .to_string();

    let date_header = response
        .headers()
        .get("date")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // Parse date for filename
    let timestamp = if !date_header.is_empty() {
        match httpdate::parse_http_date(date_header) {
            Ok(system_time) => {
                let datetime: DateTime<Utc> = system_time.into();
                datetime.format("%Y%m%d_%H%M%S").to_string()
            }
            Err(_) => {
                let now: DateTime<Utc> = Utc::now();
                now.format("%Y%m%d_%H%M%S").to_string()
            }
        }
    } else {
        let now: DateTime<Utc> = Utc::now();
        now.format("%Y%m%d_%H%M%S").to_string()
    };

    let output_filename = format!("recording_{}.wav", timestamp);

    println!("Content-Type: {}", content_type);
    println!("Output file: {}", output_filename);

    // Determine codec from content type
    let codec_hint = match content_type.as_str() {
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/aac" | "audio/aacp" | "audio/x-aac" => "aac",
        _ => {
            println!("Warning: Unknown content type '{}', assuming MP3", content_type);
            "mp3"
        }
    };

    println!("Detected codec: {}", codec_hint);
    println!("Downloading audio data...");

    // Buffer audio data for the specified duration
    let mut audio_buffer = Vec::new();
    let start_time = Instant::now();
    let duration_limit = Duration::from_secs(args.duration);

    let mut reader = response;
    let mut chunk = [0u8; 8192];

    while start_time.elapsed() < duration_limit {
        match reader.read(&mut chunk) {
            Ok(0) => {
                println!("Stream ended");
                break;
            }
            Ok(n) => {
                audio_buffer.extend_from_slice(&chunk[..n]);
            }
            Err(e) => {
                eprintln!("Read error: {}", e);
                break;
            }
        }
    }

    println!(
        "Downloaded {} bytes in {:.1} seconds",
        audio_buffer.len(),
        start_time.elapsed().as_secs_f64()
    );

    if audio_buffer.is_empty() {
        return Err("No audio data received".into());
    }

    // Decode audio using Symphonia
    println!("Decoding audio...");

    let cursor = Cursor::new(audio_buffer);
    let mss = MediaSourceStream::new(Box::new(cursor), Default::default());

    // Create a hint to help the format registry guess the format
    let mut hint = Hint::new();
    hint.with_extension(codec_hint);

    // Use the default options for format reader and metadata
    let format_opts = FormatOptions::default();
    let metadata_opts = MetadataOptions::default();

    // Probe the media source
    let probed = symphonia::default::get_probe().format(&hint, mss, &format_opts, &metadata_opts)?;

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
    let mut decoder =
        symphonia::default::get_codecs().make(&codec_params, &decoder_opts)?;

    // Get audio parameters
    let sample_rate = codec_params
        .sample_rate
        .ok_or("Unknown sample rate")?;
    let channels = codec_params
        .channels
        .map(|c| c.count())
        .unwrap_or(2) as u16;

    println!("Audio format: {} Hz, {} channels", sample_rate, channels);

    // Collect all PCM samples
    let mut all_samples: Vec<i16> = Vec::new();

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

                        // Append samples to our collection
                        all_samples.extend_from_slice(sample_buf.samples());
                    }
                    Err(symphonia::core::errors::Error::DecodeError(e)) => {
                        eprintln!("Decode error: {}", e);
                        continue;
                    }
                    Err(e) => {
                        eprintln!("Fatal decode error: {}", e);
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
                eprintln!("Format error: {}", e);
                break;
            }
        }
    }

    if all_samples.is_empty() {
        return Err("No audio samples decoded".into());
    }

    println!("Decoded {} samples", all_samples.len());

    // Write WAV file
    println!("Writing WAV file...");

    let wav_spec = WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut wav_writer = WavWriter::create(&output_filename, wav_spec)?;

    for sample in &all_samples {
        wav_writer.write_sample(*sample)?;
    }

    wav_writer.finalize()?;

    let duration_secs = all_samples.len() as f64 / (sample_rate as f64 * channels as f64);
    println!(
        "Successfully saved {} ({:.1} seconds of audio)",
        output_filename, duration_secs
    );

    Ok(())
}
