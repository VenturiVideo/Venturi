//! Decoding whole audio tracks into interleaved f32. The track sits
//! entirely in RAM; the streaming version delivers it in chunks so the mixer
//! can play its start before the decode is over.

use ffmpeg::ChannelLayout;
use ffmpeg::format::sample::{Sample, Type as SampleType};
use ffmpeg::media::Type;
use ffmpeg::software::resampling::context::Context as Resampler;
use ffmpeg_next as ffmpeg;
use std::ops::ControlFlow;
use std::path::Path;

pub struct AudioBuffer {
    pub sample_rate: u32,
    pub channels: u16,
    /// Interleaved f32, length = total_frames * channels.
    pub samples: Vec<f32>,
}

/// Decodes the N-th audio stream (order of `probe::audio_streams`).
/// `Ok(None)` if it does not exist.
pub fn decode_audio_track(
    path: &Path,
    stream_index: usize,
) -> Result<Option<AudioBuffer>, crate::MediaError> {
    let mut samples = Vec::new();
    let formats = decode_audio_streams_streaming(path, &[stream_index], None, |_, _, chunk| {
        samples.extend_from_slice(chunk);
        ControlFlow::Continue(())
    })?;
    Ok(formats[0].map(|(sample_rate, channels)| AudioBuffer {
        sample_rate,
        channels,
        samples,
    }))
}

/// Decoded from before the target of a seek and dropped: a codec needs a few
/// frames after a seek to output the same samples as a decode from the start.
const SEEK_PREROLL_SECS: f64 = 0.5;

/// Read from the start to find where each stream's frames are.
const MAX_ORIGIN_PACKETS: usize = 2000;

/// Frames decoded from the start of each stream before a seek.
const ORIGIN_FRAMES: usize = 8;

/// Timestamps further than this from the samples counted are a jump in the
/// stream (a gap, an overlap): placed by timestamp, as the video is, so a
/// decode from the start and one from a seek agree. Above the rounding of
/// millisecond timestamps.
const DISCONTINUITY_SECS: f64 = 0.005;

/// Where a decode from the start puts a frame with timestamp `ts`.
#[derive(Clone, Copy)]
struct Anchor {
    ts: i64,
    /// In input samples.
    position: u64,
    /// Samples of every frame but the first, if they are all the same: with
    /// timestamps coarser than a sample (milliseconds in Matroska) a
    /// position is snapped to them.
    frame_samples: Option<u64>,
    /// The first frame came out shorter: the decoder drops the codec delay
    /// (Opus pre-skip) from its first frame, after a seek too, without
    /// moving its timestamp.
    trims_first: bool,
}

struct StreamDecoder {
    decoder: ffmpeg::decoder::Audio,
    resampler: Resampler,
    layout: ChannelLayout,
    in_rate: u32,
    out_rate: u32,
    channels: u16,
    /// Seconds per timestamp tick.
    time_base: f64,
    /// Where the timestamps put the frames: set by a seek, or learnt from
    /// `firsts` in a decode from the start.
    anchor: Option<Anchor>,
    firsts: Vec<(Option<i64>, u64)>,
    /// After a seek, until a whole frame is placed by its timestamp.
    placing: bool,
    /// Input frame of the next sample fed to the resampler.
    fed: u64,
    /// Output frame of the next sample.
    next: u64,
}

/// Where a decoded frame goes.
enum Place {
    /// Right after the previous one.
    Counted,
    /// At this input frame, by its timestamp.
    At(u64),
    /// Nowhere: a frame after a seek without a timestamp.
    Skip,
}

impl StreamDecoder {
    fn open(
        stream: &ffmpeg::format::stream::Stream,
        out_rate: Option<u32>,
    ) -> Result<Self, crate::MediaError> {
        let time_base = f64::from(stream.time_base());
        let decoder = ffmpeg::codec::context::Context::from_parameters(stream.parameters())?
            .decoder()
            .audio()?;
        let layout = decoder_channel_layout(&decoder);
        let in_rate = decoder.rate();
        let out_rate = out_rate.unwrap_or(in_rate);
        let resampler = Resampler::get(
            decoder.format(),
            layout,
            in_rate,
            Sample::F32(SampleType::Packed),
            layout,
            out_rate,
        )?;
        Ok(Self {
            channels: decoder.channels(),
            decoder,
            resampler,
            layout,
            in_rate,
            out_rate,
            time_base,
            anchor: None,
            firsts: Vec::new(),
            placing: false,
            fed: 0,
            next: 0,
        })
    }

    fn resamples(&self) -> bool {
        self.in_rate != self.out_rate
    }

    fn fresh_resampler(&self) -> Result<Resampler, crate::MediaError> {
        Ok(Resampler::get(
            self.decoder.format(),
            self.layout,
            self.in_rate,
            Sample::F32(SampleType::Packed),
            self.layout,
            self.out_rate,
        )?)
    }

    /// Input frames from `anchor` to timestamp `ts`, snapped to whole
    /// frames when the timestamps are coarser than a sample.
    fn input_at(&self, anchor: Anchor, ts: i64) -> f64 {
        let in_rate = self.in_rate.max(1) as f64;
        let offset = (ts - anchor.ts) as f64 * self.time_base * in_rate;
        let coarse = self.time_base * in_rate > 1.0 + 1e-9;
        match anchor.frame_samples {
            Some(frame) if coarse => (offset / frame as f64).round() * frame as f64,
            _ => offset,
        }
    }

    fn place(&mut self, decoded: &ffmpeg::frame::Audio) -> Place {
        let ts = decoded.timestamp().or(decoded.pts());
        let samples = decoded.samples() as u64;
        if !self.placing {
            if self.anchor.is_none() {
                self.firsts.push((ts, samples));
                if self.firsts.len() == ORIGIN_FRAMES {
                    self.anchor = anchor(&self.firsts);
                }
            }
            let (Some(anchor), Some(ts)) = (self.anchor, ts) else {
                return Place::Counted;
            };
            let input = anchor.position as f64 + self.input_at(anchor, ts);
            let tolerance = DISCONTINUITY_SECS * self.in_rate as f64;
            return if (input - self.fed as f64).abs() > tolerance {
                Place::At(input.round().max(0.0) as u64)
            } else {
                Place::Counted
            };
        }
        let (Some(anchor), Some(ts)) = (self.anchor, ts) else {
            return Place::Skip;
        };
        let mut offset = self.input_at(anchor, ts);
        match anchor.frame_samples {
            Some(frame) => {
                if anchor.trims_first && samples < frame {
                    offset += (frame - samples) as f64;
                }
                self.placing = samples < frame;
            }
            None => self.placing = false,
        }
        Place::At((anchor.position as f64 + offset.round()).max(0.0) as u64)
    }

    /// Resamples and delivers all the frames ready in the decoder.
    fn drain(
        &mut self,
        slot: usize,
        samples: &mut Vec<f32>,
        on_chunk: &mut impl FnMut(usize, u16, u64, &[f32]) -> ControlFlow<()>,
    ) -> Result<ControlFlow<()>, crate::MediaError> {
        let mut decoded = ffmpeg::frame::Audio::empty();
        while self.decoder.receive_frame(&mut decoded).is_ok() {
            let mut input = match self.place(&decoded) {
                Place::Skip => continue,
                Place::Counted => self.fed,
                Place::At(input) => input,
            };
            if input != self.fed {
                if self.resamples() {
                    // A resampler is in phase with one fed from the start
                    // only from an input frame that falls on an output frame.
                    if self.flush(slot, samples, on_chunk)?.is_break() {
                        return Ok(ControlFlow::Break(()));
                    }
                    self.resampler = self.fresh_resampler()?;
                    let grid = u64::from(self.in_rate / gcd(self.in_rate, self.out_rate));
                    let skip = (grid - input % grid) % grid;
                    if skip >= decoded.samples() as u64 {
                        continue;
                    }
                    decoded = drop_front(&decoded, skip as usize, self.layout);
                    input += skip;
                }
                self.fed = input;
                self.next = input * u64::from(self.out_rate) / u64::from(self.in_rate.max(1));
            }
            // `Resampler::run` allocates the output as large as the input, which
            // is not enough when resampling upwards.
            let capacity =
                decoded.samples() * self.out_rate as usize / self.in_rate.max(1) as usize + 64;
            let mut resampled =
                ffmpeg::frame::Audio::new(Sample::F32(SampleType::Packed), capacity, self.layout);
            run_resampler(&mut self.resampler, &mut decoded, &mut resampled)?;
            self.fed += decoded.samples() as u64;
            let at = self.next;
            self.next = at + resampled.samples() as u64;
            if emit(slot, self.channels, at, &resampled, samples, on_chunk).is_break() {
                return Ok(ControlFlow::Break(()));
            }
        }
        Ok(ControlFlow::Continue(()))
    }

    fn flush(
        &mut self,
        slot: usize,
        samples: &mut Vec<f32>,
        on_chunk: &mut impl FnMut(usize, u16, u64, &[f32]) -> ControlFlow<()>,
    ) -> Result<ControlFlow<()>, crate::MediaError> {
        while self.resampler.delay().is_some() {
            let mut resampled =
                ffmpeg::frame::Audio::new(Sample::F32(SampleType::Packed), 4096, self.layout);
            self.resampler.flush(&mut resampled)?;
            if resampled.samples() == 0 {
                break;
            }
            let at = self.next;
            self.next = at + resampled.samples() as u64;
            if emit(slot, self.channels, at, &resampled, samples, on_chunk).is_break() {
                return Ok(ControlFlow::Break(()));
            }
        }
        Ok(ControlFlow::Continue(()))
    }
}

fn emit(
    slot: usize,
    channels: u16,
    at: u64,
    frame: &ffmpeg::frame::Audio,
    samples: &mut Vec<f32>,
    on_chunk: &mut impl FnMut(usize, u16, u64, &[f32]) -> ControlFlow<()>,
) -> ControlFlow<()> {
    samples.clear();
    append_f32(frame, samples);
    if samples.is_empty() {
        ControlFlow::Continue(())
    } else {
        on_chunk(slot, channels, at, samples)
    }
}

/// Several streams in a single read, in chunks: `on_chunk(i, channels,
/// samples)` with `i` an index into `stream_indices`; `Break` interrupts.
/// `out_rate` resamples with one swresample context per stream, without
/// discontinuities between chunks. Returns `(rate, channels)` per stream,
/// `None` if it does not exist.
pub fn decode_audio_streams_streaming(
    path: &Path,
    stream_indices: &[usize],
    out_rate: Option<u32>,
    mut on_chunk: impl FnMut(usize, u16, &[f32]) -> ControlFlow<()>,
) -> Result<Vec<Option<(u32, u16)>>, crate::MediaError> {
    decode_streams(
        path,
        stream_indices,
        out_rate,
        None,
        |slot, channels, _, chunk| on_chunk(slot, channels, chunk),
    )
}

/// Like `decode_audio_streams_streaming`, from about frame `from` of
/// `out_rate` on: it seeks there instead of reading the file from its start.
/// `on_chunk(i, channels, at, samples)` with `at` the frame of the first
/// sample where a decode from the start puts it; chunks may start before
/// `from`, and after a jump in the stream's timestamps `at` jumps too.
pub fn decode_audio_streams_from(
    path: &Path,
    stream_indices: &[usize],
    out_rate: u32,
    from: u64,
    on_chunk: impl FnMut(usize, u16, u64, &[f32]) -> ControlFlow<()>,
) -> Result<Vec<Option<(u32, u16)>>, crate::MediaError> {
    let seek_to = from as f64 / out_rate.max(1) as f64 - SEEK_PREROLL_SECS;
    let seek_to = (seek_to > 0.0).then_some(seek_to);
    decode_streams(path, stream_indices, Some(out_rate), seek_to, on_chunk)
}

fn decode_streams(
    path: &Path,
    stream_indices: &[usize],
    out_rate: Option<u32>,
    seek_to: Option<f64>,
    mut on_chunk: impl FnMut(usize, u16, u64, &[f32]) -> ControlFlow<()>,
) -> Result<Vec<Option<(u32, u16)>>, crate::MediaError> {
    crate::probe::ensure_init();

    let mut ictx = ffmpeg::format::input(&path)?;
    let audio_streams: Vec<_> = ictx
        .streams()
        .filter(|s| s.parameters().medium() == Type::Audio)
        .collect();
    // (index in the container, decoder) per slot.
    let mut decoders: Vec<Option<(usize, StreamDecoder)>> = stream_indices
        .iter()
        .map(|&i| {
            audio_streams
                .get(i)
                .map(|stream| Ok((stream.index(), StreamDecoder::open(stream, out_rate)?)))
                .transpose()
        })
        .collect::<Result<_, crate::MediaError>>()?;
    drop(audio_streams);
    let formats = decoders
        .iter()
        .map(|d| d.as_ref().map(|(_, d)| (d.out_rate, d.channels)))
        .collect();
    if decoders.iter().all(Option::is_none) {
        return Ok(formats);
    }
    if let Some(secs) = seek_to {
        seek(&mut ictx, &mut decoders, out_rate, secs)?;
    }

    let mut samples = Vec::new();
    loop {
        // `Packet::read` does not unref the previous packet: reusing one
        // would leak every packet read.
        let mut packet = ffmpeg::Packet::empty();
        match packet.read(&mut ictx) {
            Ok(()) => {
                let Some(slot) = slot_of(&decoders, packet.stream()) else {
                    continue;
                };
                let (_, stream) = decoders[slot]
                    .as_mut()
                    .expect("slot_of returns a live slot");
                stream.decoder.send_packet(&packet)?;
                if stream.drain(slot, &mut samples, &mut on_chunk)?.is_break() {
                    return Ok(formats);
                }
            }
            Err(ffmpeg::Error::Eof) => break,
            Err(e) => return Err(e.into()),
        }
    }
    for (slot, entry) in decoders.iter_mut().enumerate() {
        let Some((_, stream)) = entry else {
            continue;
        };
        stream.decoder.send_eof()?;
        if stream.drain(slot, &mut samples, &mut on_chunk)?.is_break()
            || stream.flush(slot, &mut samples, &mut on_chunk)?.is_break()
        {
            break;
        }
    }
    Ok(formats)
}

/// Slot of the open decoder reading `stream_index`, if any.
fn slot_of(decoders: &[Option<(usize, StreamDecoder)>], stream_index: usize) -> Option<usize> {
    decoders
        .iter()
        .position(|d| d.as_ref().is_some_and(|(i, _)| *i == stream_index))
}

/// Seeks to `secs` after the first frame, with fresh decoders that place
/// their frames by timestamp, anchored on the first frames from the start.
fn seek(
    ictx: &mut ffmpeg::format::context::Input,
    decoders: &mut [Option<(usize, StreamDecoder)>],
    out_rate: Option<u32>,
    secs: f64,
) -> Result<(), crate::MediaError> {
    // (timestamp, samples) of the first frames of every stream.
    let mut firsts: Vec<Vec<(Option<i64>, u64)>> = decoders.iter().map(|_| Vec::new()).collect();
    let mut decoded = ffmpeg::frame::Audio::empty();
    for _ in 0..MAX_ORIGIN_PACKETS {
        let mut packet = ffmpeg::Packet::empty();
        if packet.read(ictx).is_err() {
            break;
        }
        let Some(slot) = slot_of(decoders, packet.stream()) else {
            continue;
        };
        let (Some((_, stream)), frames) = (&mut decoders[slot], &mut firsts[slot]) else {
            continue;
        };
        if frames.len() < ORIGIN_FRAMES && stream.decoder.send_packet(&packet).is_ok() {
            while stream.decoder.receive_frame(&mut decoded).is_ok() {
                frames.push((
                    decoded.timestamp().or(decoded.pts()),
                    decoded.samples() as u64,
                ));
            }
        }
        if decoders
            .iter()
            .zip(&firsts)
            .all(|(d, frames)| d.is_none() || frames.len() >= ORIGIN_FRAMES)
        {
            break;
        }
    }
    let mut first_secs = f64::INFINITY;
    for (entry, frames) in decoders.iter_mut().zip(&firsts) {
        let Some((index, old)) = entry else {
            continue;
        };
        let stream = ictx.stream(*index).ok_or(ffmpeg::Error::StreamNotFound)?;
        let mut fresh = StreamDecoder::open(&stream, out_rate)?;
        fresh.anchor = anchor(frames).or_else(|| {
            eprintln!(
                "audio seek: no anchor frames in stream {index}, \
                 placing from its start time (may be off by up to a frame)"
            );
            Some(Anchor {
                ts: stream.start_time().max(0),
                position: 0,
                frame_samples: None,
                trims_first: false,
            })
        });
        fresh.placing = true;
        if let Some(anchor) = fresh.anchor {
            let start = anchor.ts as f64 * fresh.time_base
                - anchor.position as f64 / fresh.in_rate.max(1) as f64;
            first_secs = first_secs.min(start);
        }
        *old = fresh;
    }
    let target = ((first_secs.max(0.0) + secs) * f64::from(ffmpeg::ffi::AV_TIME_BASE)) as i64;
    // Some demuxers refuse a target past the end: from the start, the
    // frames are still placed by timestamp.
    if ictx.seek(target, ..target).is_err() {
        ictx.seek(0, ..0)?;
    }
    Ok(())
}

/// From the first frames of a stream: the second one sits where the first
/// one's samples end.
fn anchor(frames: &[(Option<i64>, u64)]) -> Option<Anchor> {
    match frames {
        [(Some(ts), _)] => Some(Anchor {
            ts: *ts,
            position: 0,
            frame_samples: None,
            trims_first: false,
        }),
        [(_, first), (Some(ts), _), rest @ ..] => {
            // The last one read may be the end of the stream, shorter.
            let others = &frames[1..frames.len() - usize::from(!rest.is_empty())];
            let samples = others[0].1;
            let fixed = others.len() >= 2 && others.iter().all(|&(_, n)| n == samples);
            Some(Anchor {
                ts: *ts,
                position: *first,
                frame_samples: fixed.then_some(samples),
                trims_first: fixed && *first < samples,
            })
        }
        _ => None,
    }
}

/// Decoder channel layout, with a default one in place of an unspecified
/// one (e.g. PCM in MKV): otherwise swresample rejects every frame with
/// "Input changed".
pub(crate) fn decoder_channel_layout(decoder: &ffmpeg::decoder::Audio) -> ChannelLayout {
    let layout = decoder.channel_layout();
    if layout.is_empty() {
        ChannelLayout::default(i32::from(decoder.channels()))
    } else {
        layout
    }
}

/// `Resampler::run` after aligning the frame layout to the resampler's
/// (see `decoder_channel_layout`).
pub(crate) fn run_resampler(
    resampler: &mut Resampler,
    decoded: &mut ffmpeg::frame::Audio,
    resampled: &mut ffmpeg::frame::Audio,
) -> Result<(), crate::MediaError> {
    if decoded.channel_layout().is_empty() {
        decoded.set_channel_layout(resampler.input().channel_layout);
    }
    resampler.run(decoded, resampled)?;
    Ok(())
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a.max(1) } else { gcd(b, a % b) }
}

/// `frame` without its first `skip` samples. `layout`: see
/// `decoder_channel_layout`.
fn drop_front(
    frame: &ffmpeg::frame::Audio,
    skip: usize,
    layout: ChannelLayout,
) -> ffmpeg::frame::Audio {
    let samples = frame.samples() - skip;
    let mut out = ffmpeg::frame::Audio::new(frame.format(), samples, layout);
    out.set_rate(frame.rate());
    let sample_bytes = frame.format().bytes();
    let (planes, plane_samples) = if frame.is_planar() {
        (frame.planes(), 1)
    } else {
        (1, frame.channels() as usize)
    };
    for plane in 0..planes {
        let from = skip * plane_samples * sample_bytes;
        let len = samples * plane_samples * sample_bytes;
        out.data_mut(plane)[..len].copy_from_slice(&frame.data(plane)[from..from + len]);
    }
    out
}

fn append_f32(frame: &ffmpeg::frame::Audio, out: &mut Vec<f32>) {
    let byte_len = frame.samples() * frame.channels() as usize * 4;
    out.extend(
        frame.data(0)[..byte_len]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_ne_bytes(*b)),
    );
}

#[cfg(test)]
#[path = "tests/audio.rs"]
mod tests;
