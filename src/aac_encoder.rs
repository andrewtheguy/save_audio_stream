//! The AAC-LC encoder, which only a build with the non-default `aac` feature has.
//!
//! Fraunhofer FDK AAC is the only practical AAC encoder for Rust without FFmpeg. Its
//! licence is not OSI-approved and grants no patent rights, so no release artifact
//! links it: recording AAC takes `cargo build --release --features aac`, and any
//! other build refuses a session that asks for it. Serving and syncing AAC that is
//! already recorded needs no encoder and works in every build.

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// What gapless playback needs to know of the encoder, stored beside the recording.
pub struct AacEncoderInfo {
    pub delay: u32,
    pub frame_length: u32,
}

/// Whether this build can record AAC, as the error a session is refused with.
pub fn require() -> Result<(), BoxError> {
    if cfg!(feature = "aac") {
        Ok(())
    } else {
        Err("this build has no AAC encoder: build with `--features aac`, or record opus".into())
    }
}

#[cfg(feature = "aac")]
pub struct AacEncoder(fdk_aac::enc::Encoder);

#[cfg(feature = "aac")]
impl AacEncoder {
    /// 16 kHz mono AAC-LC in ADTS, at a constant `bit_rate` in bits per second.
    pub fn new(bit_rate: u32) -> Result<Self, BoxError> {
        use fdk_aac::enc::{
            AudioObjectType, BitRate, ChannelMode, Encoder, EncoderParams, Transport,
        };

        let params = EncoderParams {
            bit_rate: BitRate::Cbr(bit_rate),
            sample_rate: 16000,
            channels: ChannelMode::Mono,
            transport: Transport::Adts,
            audio_object_type: AudioObjectType::Mpeg4LowComplexity,
        };
        Encoder::new(params)
            .map(Self)
            .map_err(|e| format!("Failed to create AAC encoder: {:?}", e).into())
    }

    pub fn info(&self) -> Result<AacEncoderInfo, BoxError> {
        let info = self.0.info()?;
        Ok(AacEncoderInfo {
            delay: info.nDelay,
            frame_length: info.frameLength,
        })
    }

    /// Encode one frame into `output`, returning how many bytes of it were written.
    pub fn encode(&mut self, frame: &[i16], output: &mut [u8]) -> Result<usize, BoxError> {
        Ok(self.0.encode(frame, output)?.output_size)
    }
}

/// Uninhabited: `new` is the only way to one, and without the feature it fails.
#[cfg(not(feature = "aac"))]
pub enum AacEncoder {}

#[cfg(not(feature = "aac"))]
impl AacEncoder {
    pub fn new(_bit_rate: u32) -> Result<Self, BoxError> {
        require().map(|()| unreachable!())
    }

    pub fn info(&self) -> Result<AacEncoderInfo, BoxError> {
        match *self {}
    }

    pub fn encode(&mut self, _frame: &[i16], _output: &mut [u8]) -> Result<usize, BoxError> {
        match *self {}
    }
}
