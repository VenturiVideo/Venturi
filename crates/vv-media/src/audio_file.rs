//! Audio-only files, as the voiceover takes are saved.

use ffmpeg::codec::{self, encoder};
use ffmpeg::format;
use ffmpeg::format::sample::{Sample, Type as SampleType};
use ffmpeg::software::resampling::context::Context as Resampler;
use ffmpeg::{ChannelLayout, frame};
use ffmpeg_next as ffmpeg;
use std::path::Path;

use crate::MediaError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioFileFormat {
    Mp3,
    Wav,
    Flac,
    Aac,
    Opus,
    Vorbis,
}

impl AudioFileFormat {
    pub const ALL: [Self; 6] = [
        Self::Mp3,
        Self::Wav,
        Self::Flac,
        Self::Aac,
        Self::Opus,
        Self::Vorbis,
    ];

    /// Key in the settings file: must never be changed.
    pub fn id(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Wav => "wav",
            Self::Flac => "flac",
            Self::Aac => "aac",
            Self::Opus => "opus",
            Self::Vorbis => "vorbis",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Mp3 => "MP3",
            Self::Wav => "WAV",
            Self::Flac => "FLAC",
            Self::Aac => "AAC (.m4a)",
            Self::Opus => "Opus",
            Self::Vorbis => "Ogg Vorbis",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Wav => "wav",
            Self::Flac => "flac",
            Self::Aac => "m4a",
            Self::Opus => "opus",
            Self::Vorbis => "ogg",
        }
    }

    fn encoder_name(self) -> &'static str {
        match self {
            Self::Mp3 => "libmp3lame",
            Self::Wav => "pcm_f32le",
            Self::Flac => "flac",
            Self::Aac => "aac",
            Self::Opus => "libopus",
            Self::Vorbis => "libvorbis",
        }
    }

    /// Of the lossy ones; the others ignore it.
    fn bit_rate(self) -> usize {
        match self {
            Self::Opus => 128_000,
            _ => 192_000,
        }
    }

    /// Whether the FFmpeg in use can write it: the builds differ.
    pub fn is_available(self) -> bool {
        crate::probe::ensure_init();
        encoder::find_by_name(self.encoder_name()).is_some()
    }

    pub fn available() -> Vec<Self> {
        Self::ALL.into_iter().filter(|f| f.is_available()).collect()
    }

    /// `preferred` if this FFmpeg has it, otherwise MP3, otherwise WAV (always
    /// there: it is not compressed).
    pub fn resolve(preferred: Option<Self>) -> Self {
        [preferred, Some(Self::Mp3)]
            .into_iter()
            .flatten()
            .find(|f| f.is_available())
            .unwrap_or(Self::Wav)
    }
}

/// Writes `samples` (interleaved, `channels` channels at `sample_rate`) to
/// `path` in `format`, resampling if the encoder does not take that rate
/// (Opus: 48 kHz).
pub fn write_audio_file(
    path: &Path,
    format: AudioFileFormat,
    samples: &[f32],
    sample_rate: u32,
    channels: u16,
) -> Result<(), MediaError> {
    crate::probe::ensure_init();
    let ch = channels.max(1);
    let codec = encoder::find_by_name(format.encoder_name()).ok_or_else(|| {
        MediaError::NoStream(format!("encoder {} not available", format.encoder_name()))
    })?;
    let audio_codec = codec.audio()?;
    let rate = match audio_codec.rates() {
        Some(rates) => {
            let rates: Vec<i32> = rates.collect();
            if rates.contains(&(sample_rate as i32)) {
                sample_rate
            } else {
                rates.into_iter().max().unwrap_or(48_000) as u32
            }
        }
        None => sample_rate,
    };
    let layout = ChannelLayout::default(ch as i32);
    let samples = to_rate(samples, ch, sample_rate, rate, layout)?;

    let mut octx = format::output(path)?;
    let global_header = octx.format().flags().contains(format::Flags::GLOBAL_HEADER);
    let stream_index = octx.add_stream(codec)?.index();
    let mut ctx = codec::context::Context::new_with_codec(codec)
        .encoder()
        .audio()?;
    ctx.set_rate(rate as i32);
    ctx.set_channel_layout(layout);
    ctx.set_bit_rate(format.bit_rate());
    let sample_format = audio_codec
        .formats()
        .and_then(|mut formats| formats.next())
        .ok_or_else(|| {
            MediaError::NoStream(format!("{} has no sample format", format.encoder_name()))
        })?;
    ctx.set_format(sample_format);
    let time_base = ffmpeg::Rational::new(1, rate as i32);
    ctx.set_time_base(time_base);
    if global_header {
        ctx.set_flags(codec::Flags::GLOBAL_HEADER);
    }
    let mut encoder = ctx.open_as(codec)?;
    octx.stream_mut(stream_index)
        .expect("just added")
        .set_parameters(&encoder);
    octx.write_header()?;
    let ost_time_base = octx.stream(stream_index).expect("just added").time_base();

    let packed = Sample::F32(SampleType::Packed);
    let mut convert = Resampler::get(
        packed,
        layout,
        rate,
        encoder.format(),
        encoder.channel_layout(),
        rate,
    )?;
    // Encoders with a variable frame size report 0.
    let frame_size = match encoder.frame_size() {
        0 => 4096,
        n => n as usize,
    };
    let short_last_frame = codec.capabilities().intersects(
        codec::Capabilities::VARIABLE_FRAME_SIZE | codec::Capabilities::SMALL_LAST_FRAME,
    );
    let mut pts = 0i64;
    for chunk in samples.chunks(frame_size * ch as usize) {
        let mut chunk = chunk.to_vec();
        if !short_last_frame {
            chunk.resize(frame_size * ch as usize, 0.0);
        }
        let input = packed_frame(&chunk, ch, rate, layout);
        let mut converted = frame::Audio::empty();
        convert.run(&input, &mut converted)?;
        converted.set_pts(Some(pts));
        pts += converted.samples() as i64;
        encoder.send_frame(&converted)?;
        crate::encode::drain_packets(
            &mut octx,
            &mut encoder,
            stream_index,
            time_base,
            ost_time_base,
        )?;
    }
    encoder.send_eof()?;
    crate::encode::drain_packets(
        &mut octx,
        &mut encoder,
        stream_index,
        time_base,
        ost_time_base,
    )?;
    octx.write_trailer()?;
    Ok(())
}

fn packed_frame(samples: &[f32], channels: u16, rate: u32, layout: ChannelLayout) -> frame::Audio {
    let frames = samples.len() / channels.max(1) as usize;
    let mut frame = frame::Audio::new(Sample::F32(SampleType::Packed), frames, layout);
    frame.set_rate(rate);
    let (bytes, _) = frame.data_mut(0).as_chunks_mut::<4>();
    for (b, s) in bytes.iter_mut().zip(samples) {
        *b = s.to_ne_bytes();
    }
    frame
}

fn packed_samples(frame: &frame::Audio, channels: u16) -> impl Iterator<Item = f32> + '_ {
    let len = frame.samples() * channels as usize;
    frame.data(0)[..len * 4]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_ne_bytes(*b))
}

/// `samples` from `from` to `to` Hz, with the resampler's tail flushed.
fn to_rate(
    samples: &[f32],
    channels: u16,
    from: u32,
    to: u32,
    layout: ChannelLayout,
) -> Result<Vec<f32>, MediaError> {
    if from == to {
        return Ok(samples.to_vec());
    }
    let packed = Sample::F32(SampleType::Packed);
    let mut resampler = Resampler::get(packed, layout, from, packed, layout, to)?;
    let input = packed_frame(samples, channels, from, layout);
    // Allocated here and large enough: `run` would size it on the input,
    // too short when going up.
    let frames = input.samples() as u64 * to as u64 / from as u64 + 1024;
    let mut out = frame::Audio::new(packed, frames as usize, layout);
    resampler.run(&input, &mut out)?;
    let mut result: Vec<f32> = packed_samples(&out, channels).collect();
    let mut tail = frame::Audio::new(packed, 4096, layout);
    resampler.flush(&mut tail)?;
    result.extend(packed_samples(&tail, channels));
    Ok(result)
}

#[cfg(test)]
#[path = "tests/audio_file.rs"]
mod tests;
