use std::io;

/// Write a 32-bit big-endian integer
fn write_u32_be(writer: &mut Vec<u8>, value: u32) {
    writer.extend_from_slice(&value.to_be_bytes());
}

/// Write a 16-bit big-endian integer
fn write_u16_be(writer: &mut Vec<u8>, value: u16) {
    writer.extend_from_slice(&value.to_be_bytes());
}

/// Write a 64-bit big-endian integer
fn write_u64_be(writer: &mut Vec<u8>, value: u64) {
    writer.extend_from_slice(&value.to_be_bytes());
}

/// Write an MP4 box with size and type
fn write_box<F>(writer: &mut Vec<u8>, box_type: &[u8; 4], content_fn: F) -> io::Result<()>
where
    F: FnOnce(&mut Vec<u8>) -> io::Result<()>,
{
    let start_pos = writer.len();
    write_u32_be(writer, 0); // Placeholder for size
    writer.extend_from_slice(box_type);
    content_fn(writer)?;
    let end_pos = writer.len();
    let size = (end_pos - start_pos) as u32;
    writer[start_pos..start_pos + 4].copy_from_slice(&size.to_be_bytes());
    Ok(())
}

/// Write ftyp box (file type)
fn write_ftyp(writer: &mut Vec<u8>) -> io::Result<()> {
    write_box(writer, b"ftyp", |w| {
        w.extend_from_slice(b"iso5"); // Major brand: ISO Base Media v5
        write_u32_be(w, 0); // Minor version
        w.extend_from_slice(b"iso5"); // Compatible brand
        w.extend_from_slice(b"iso6"); // Compatible brand
        w.extend_from_slice(b"mp41"); // Compatible brand
        Ok(())
    })
}

/// Write mvhd box (movie header)
fn write_mvhd(writer: &mut Vec<u8>, timescale: u32) -> io::Result<()> {
    write_box(writer, b"mvhd", |w| {
        w.push(1); // Version 1
        w.extend_from_slice(&[0, 0, 0]); // Flags
        write_u64_be(w, 0); // Creation time
        write_u64_be(w, 0); // Modification time
        write_u32_be(w, timescale); // Timescale (48000 for 48kHz)
        write_u64_be(w, 0); // Duration (unknown for live stream)
        write_u32_be(w, 0x00010000); // Rate (1.0)
        write_u16_be(w, 0x0100); // Volume (1.0)
        write_u16_be(w, 0); // Reserved
        write_u32_be(w, 0); // Reserved
        write_u32_be(w, 0); // Reserved
                            // Matrix
        for &val in &[0x00010000, 0, 0, 0, 0x00010000, 0, 0, 0, 0x40000000] {
            write_u32_be(w, val);
        }
        // Pre-defined
        for _ in 0..6 {
            write_u32_be(w, 0);
        }
        write_u32_be(w, 2); // Next track ID
        Ok(())
    })
}

/// Write tkhd box (track header)
fn write_tkhd(writer: &mut Vec<u8>, track_id: u32) -> io::Result<()> {
    write_box(writer, b"tkhd", |w| {
        w.push(1); // Version 1
        w.extend_from_slice(&[0, 0, 7]); // Flags: track enabled, in movie, in preview
        write_u64_be(w, 0); // Creation time
        write_u64_be(w, 0); // Modification time
        write_u32_be(w, track_id); // Track ID
        write_u32_be(w, 0); // Reserved
        write_u64_be(w, 0); // Duration (unknown)
        write_u32_be(w, 0); // Reserved
        write_u32_be(w, 0); // Reserved
        write_u16_be(w, 0); // Layer
        write_u16_be(w, 0); // Alternate group
        write_u16_be(w, 0x0100); // Volume (1.0)
        write_u16_be(w, 0); // Reserved
                            // Matrix
        for &val in &[0x00010000, 0, 0, 0, 0x00010000, 0, 0, 0, 0x40000000] {
            write_u32_be(w, val);
        }
        write_u32_be(w, 0); // Width (not applicable for audio)
        write_u32_be(w, 0); // Height (not applicable for audio)
        Ok(())
    })
}

/// Write mdhd box (media header)
fn write_mdhd(writer: &mut Vec<u8>, timescale: u32) -> io::Result<()> {
    write_box(writer, b"mdhd", |w| {
        w.push(1); // Version 1
        w.extend_from_slice(&[0, 0, 0]); // Flags
        write_u64_be(w, 0); // Creation time
        write_u64_be(w, 0); // Modification time
        write_u32_be(w, timescale); // Timescale (48000 for 48kHz)
        write_u64_be(w, 0); // Duration (unknown)
        write_u16_be(w, 0x55c4); // Language: "und" (undetermined)
        write_u16_be(w, 0); // Pre-defined
        Ok(())
    })
}

/// Write hdlr box (handler reference)
fn write_hdlr(writer: &mut Vec<u8>) -> io::Result<()> {
    write_box(writer, b"hdlr", |w| {
        w.push(0); // Version
        w.extend_from_slice(&[0, 0, 0]); // Flags
        write_u32_be(w, 0); // Pre-defined
        w.extend_from_slice(b"soun"); // Handler type: sound
        write_u32_be(w, 0); // Reserved
        write_u32_be(w, 0); // Reserved
        write_u32_be(w, 0); // Reserved
        w.extend_from_slice(b"SoundHandler\0"); // Name
        Ok(())
    })
}

/// Write smhd box (sound media header)
fn write_smhd(writer: &mut Vec<u8>) -> io::Result<()> {
    write_box(writer, b"smhd", |w| {
        w.push(0); // Version
        w.extend_from_slice(&[0, 0, 0]); // Flags
        write_u16_be(w, 0); // Balance
        write_u16_be(w, 0); // Reserved
        Ok(())
    })
}

/// Write dref box (data reference)
fn write_dref(writer: &mut Vec<u8>) -> io::Result<()> {
    write_box(writer, b"dref", |w| {
        w.push(0); // Version
        w.extend_from_slice(&[0, 0, 0]); // Flags
        write_u32_be(w, 1); // Entry count

        // url box
        write_box(w, b"url ", |u| {
            u.push(0); // Version
            u.extend_from_slice(&[0, 0, 1]); // Flags: self-contained
            Ok(())
        })?;
        Ok(())
    })
}

/// Write dinf box (data information)
fn write_dinf(writer: &mut Vec<u8>) -> io::Result<()> {
    write_box(writer, b"dinf", |w| write_dref(w))
}

/// Write dOps box (Opus Specific Box)
fn write_dops(writer: &mut Vec<u8>, channel_count: u8, sample_rate: u32) -> io::Result<()> {
    write_box(writer, b"dOps", |w| {
        w.push(0); // Version
        w.push(channel_count); // OutputChannelCount
        write_u16_be(w, 0); // PreSkip (in samples at 48kHz)
        write_u32_be(w, sample_rate); // InputSampleRate (original sample rate)
        write_u16_be(w, 0); // OutputGain (in 1/256 dB)
        w.push(0); // ChannelMappingFamily
        Ok(())
    })
}

/// Write Opus sample entry
fn write_opus_sample_entry(
    writer: &mut Vec<u8>,
    channel_count: u16,
    sample_rate: u32,
) -> io::Result<()> {
    write_box(writer, b"Opus", |w| {
        // SampleEntry fields
        w.extend_from_slice(&[0; 6]); // Reserved
        write_u16_be(w, 1); // Data reference index

        // AudioSampleEntry fields
        write_u32_be(w, 0); // Reserved
        write_u32_be(w, 0); // Reserved
        write_u16_be(w, channel_count); // Channel count
        write_u16_be(w, 16); // Sample size (16 bits)
        write_u16_be(w, 0); // Pre-defined
        write_u16_be(w, 0); // Reserved
        write_u32_be(w, (sample_rate as u32) << 16); // Sample rate (16.16 fixed point)

        // Opus specific box
        write_dops(w, channel_count as u8, sample_rate)?;
        Ok(())
    })
}

/// Write stsd box (sample description)
fn write_stsd(writer: &mut Vec<u8>, channel_count: u16, sample_rate: u32) -> io::Result<()> {
    write_box(writer, b"stsd", |w| {
        w.push(0); // Version
        w.extend_from_slice(&[0, 0, 0]); // Flags
        write_u32_be(w, 1); // Entry count
        write_opus_sample_entry(w, channel_count, sample_rate)?;
        Ok(())
    })
}

/// Write stts box (time to sample) - empty for fragmented MP4
fn write_stts(writer: &mut Vec<u8>) -> io::Result<()> {
    write_box(writer, b"stts", |w| {
        w.push(0); // Version
        w.extend_from_slice(&[0, 0, 0]); // Flags
        write_u32_be(w, 0); // Entry count
        Ok(())
    })
}

/// Write stsc box (sample to chunk) - empty for fragmented MP4
fn write_stsc(writer: &mut Vec<u8>) -> io::Result<()> {
    write_box(writer, b"stsc", |w| {
        w.push(0); // Version
        w.extend_from_slice(&[0, 0, 0]); // Flags
        write_u32_be(w, 0); // Entry count
        Ok(())
    })
}

/// Write stsz box (sample sizes) - empty for fragmented MP4
fn write_stsz(writer: &mut Vec<u8>) -> io::Result<()> {
    write_box(writer, b"stsz", |w| {
        w.push(0); // Version
        w.extend_from_slice(&[0, 0, 0]); // Flags
        write_u32_be(w, 0); // Sample size
        write_u32_be(w, 0); // Sample count
        Ok(())
    })
}

/// Write stco box (chunk offsets) - empty for fragmented MP4
fn write_stco(writer: &mut Vec<u8>) -> io::Result<()> {
    write_box(writer, b"stco", |w| {
        w.push(0); // Version
        w.extend_from_slice(&[0, 0, 0]); // Flags
        write_u32_be(w, 0); // Entry count
        Ok(())
    })
}

/// Write stbl box (sample table)
fn write_stbl(writer: &mut Vec<u8>, channel_count: u16, sample_rate: u32) -> io::Result<()> {
    write_box(writer, b"stbl", |w| {
        write_stsd(w, channel_count, sample_rate)?;
        write_stts(w)?;
        write_stsc(w)?;
        write_stsz(w)?;
        write_stco(w)?;
        Ok(())
    })
}

/// Write minf box (media information)
fn write_minf(writer: &mut Vec<u8>, channel_count: u16, sample_rate: u32) -> io::Result<()> {
    write_box(writer, b"minf", |w| {
        write_smhd(w)?;
        write_dinf(w)?;
        write_stbl(w, channel_count, sample_rate)?;
        Ok(())
    })
}

/// Write mdia box (media)
fn write_mdia(
    writer: &mut Vec<u8>,
    timescale: u32,
    channel_count: u16,
    sample_rate: u32,
) -> io::Result<()> {
    write_box(writer, b"mdia", |w| {
        write_mdhd(w, timescale)?;
        write_hdlr(w)?;
        write_minf(w, channel_count, sample_rate)?;
        Ok(())
    })
}

/// Write mvex box (movie extends) for fragmented MP4
fn write_mvex(writer: &mut Vec<u8>, track_id: u32) -> io::Result<()> {
    write_box(writer, b"mvex", |w| {
        // trex (track extends)
        write_box(w, b"trex", |t| {
            t.push(0); // Version
            t.extend_from_slice(&[0, 0, 0]); // Flags
            write_u32_be(t, track_id); // Track ID
            write_u32_be(t, 1); // Default sample description index
            write_u32_be(t, 0); // Default sample duration
            write_u32_be(t, 0); // Default sample size
            write_u32_be(t, 0); // Default sample flags
            Ok(())
        })?;
        Ok(())
    })
}

/// Write trak box (track)
fn write_trak(
    writer: &mut Vec<u8>,
    track_id: u32,
    timescale: u32,
    channel_count: u16,
    sample_rate: u32,
) -> io::Result<()> {
    write_box(writer, b"trak", |w| {
        write_tkhd(w, track_id)?;
        write_mdia(w, timescale, channel_count, sample_rate)?;
        Ok(())
    })
}

/// Write moov box (movie metadata)
fn write_moov(
    writer: &mut Vec<u8>,
    timescale: u32,
    track_id: u32,
    channel_count: u16,
    sample_rate: u32,
) -> io::Result<()> {
    write_box(writer, b"moov", |w| {
        write_mvhd(w, timescale)?;
        write_trak(w, track_id, timescale, channel_count, sample_rate)?;
        write_mvex(w, track_id)?;
        Ok(())
    })
}

/// Write mfhd box (movie fragment header)
fn write_mfhd(writer: &mut Vec<u8>, sequence_number: u32) -> io::Result<()> {
    write_box(writer, b"mfhd", |w| {
        w.push(0); // Version
        w.extend_from_slice(&[0, 0, 0]); // Flags
        write_u32_be(w, sequence_number); // Sequence number
        Ok(())
    })
}

/// Write tfhd box (track fragment header)
fn write_tfhd(writer: &mut Vec<u8>, track_id: u32) -> io::Result<()> {
    write_box(writer, b"tfhd", |w| {
        w.push(0); // Version
                   // Flags: default-base-is-moof (so offsets are relative to this moof box)
        w.extend_from_slice(&[0x02, 0, 0]);
        write_u32_be(w, track_id); // Track ID
        Ok(())
    })
}

/// Write tfdt box (track fragment decode time)
fn write_tfdt(writer: &mut Vec<u8>, base_media_decode_time: u64) -> io::Result<()> {
    write_box(writer, b"tfdt", |w| {
        w.push(1); // Version 1 (for 64-bit time)
        w.extend_from_slice(&[0, 0, 0]); // Flags
        write_u64_be(w, base_media_decode_time); // Base media decode time
        Ok(())
    })
}

/// Write trun box (track fragment run)
fn write_trun(
    writer: &mut Vec<u8>,
    sample_count: u32,
    sample_sizes: &[u32],
    sample_durations: &[u32],
) -> io::Result<usize> {
    let mut data_offset_pos = 0usize;
    write_box(writer, b"trun", |w| {
        w.push(0); // Version
                   // Flags: data-offset-present, sample-duration-present, sample-size-present
        w.extend_from_slice(&[0, 0x03, 0x01]); // Flags
        write_u32_be(w, sample_count); // Sample count
        data_offset_pos = w.len();
        write_u32_be(w, 0); // Data offset patched later

        for i in 0..sample_count as usize {
            write_u32_be(w, sample_durations[i]); // Sample duration
            write_u32_be(w, sample_sizes[i]); // Sample size
        }
        Ok(())
    })?;
    Ok(data_offset_pos)
}

/// Write traf box (track fragment)
fn write_traf(
    writer: &mut Vec<u8>,
    track_id: u32,
    base_media_decode_time: u64,
    sample_sizes: &[u32],
    sample_durations: &[u32],
) -> io::Result<usize> {
    let mut data_offset_pos = 0usize;
    write_box(writer, b"traf", |w| {
        write_tfhd(w, track_id)?;
        write_tfdt(w, base_media_decode_time)?;
        data_offset_pos = write_trun(w, sample_sizes.len() as u32, sample_sizes, sample_durations)?;
        Ok(())
    })?;
    Ok(data_offset_pos)
}

/// Write moof box (movie fragment)
fn write_moof(
    writer: &mut Vec<u8>,
    sequence_number: u32,
    track_id: u32,
    base_media_decode_time: u64,
    sample_sizes: &[u32],
    sample_durations: &[u32],
) -> io::Result<usize> {
    let mut data_offset_pos = 0usize;
    write_box(writer, b"moof", |w| {
        write_mfhd(w, sequence_number)?;
        data_offset_pos = write_traf(
            w,
            track_id,
            base_media_decode_time,
            sample_sizes,
            sample_durations,
        )?;
        Ok(())
    })?;
    Ok(data_offset_pos)
}

/// Write mdat box (media data)
fn write_mdat(writer: &mut Vec<u8>, data: &[u8]) -> io::Result<()> {
    write_box(writer, b"mdat", |w| {
        w.extend_from_slice(data);
        Ok(())
    })
}

/// Generate fMP4 initialization segment (ftyp + moov)
pub fn generate_init_segment(
    timescale: u32,
    track_id: u32,
    channel_count: u16,
    sample_rate: u32,
) -> Result<Vec<u8>, io::Error> {
    let mut buffer = Vec::new();
    write_ftyp(&mut buffer)?;
    write_moov(&mut buffer, timescale, track_id, channel_count, sample_rate)?;
    Ok(buffer)
}

/// Generate fMP4 media segment (moof + mdat)
///
/// # Arguments
/// * `sequence_number` - Fragment sequence number (usually segment ID)
/// * `track_id` - Track ID (should match init segment)
/// * `base_media_decode_time` - Decode time for this fragment in timescale units
/// * `opus_packets` - Vector of Opus packet data
/// * `timescale` - Media timescale (48000 for 48kHz)
/// * `samples_per_packet` - Samples per Opus packet (960 for 20ms frames at 48kHz)
pub fn generate_media_segment(
    sequence_number: u32,
    track_id: u32,
    base_media_decode_time: u64,
    opus_packets: &[Vec<u8>],
    _timescale: u32,
    samples_per_packet: u32,
) -> Result<Vec<u8>, io::Error> {
    let mut buffer = Vec::new();

    // Calculate sample sizes and durations
    let sample_sizes: Vec<u32> = opus_packets.iter().map(|p| p.len() as u32).collect();
    let sample_durations: Vec<u32> = vec![samples_per_packet; opus_packets.len()];

    // Concatenate all packet data for mdat
    let mut media_data = Vec::new();
    for packet in opus_packets {
        media_data.extend_from_slice(packet);
    }

    let data_offset_pos = write_moof(
        &mut buffer,
        sequence_number,
        track_id,
        base_media_decode_time,
        &sample_sizes,
        &sample_durations,
    )?;

    // Patch trun data offset so decoders know where media starts relative to this moof
    let moof_size = buffer.len();
    let data_offset = (moof_size + 8) as u32; // mdat header is 8 bytes
    if data_offset_pos + 4 <= buffer.len() {
        buffer[data_offset_pos..data_offset_pos + 4].copy_from_slice(&data_offset.to_be_bytes());
    }

    write_mdat(&mut buffer, &media_data)?;

    Ok(buffer)
}
