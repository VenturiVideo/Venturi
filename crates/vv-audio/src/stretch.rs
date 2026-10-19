//! Pitch-preserving time-stretch with libavfilter's `rubberband` filter.
//! It works on windows of a few seconds (`vv-app::timeline_audio`), not on
//! the whole track: stretching everything would delay the first sound by
//! seconds.

use ffmpeg::channel_layout::ChannelLayout;
use ffmpeg::format::sample::{Sample as SampleFormat, Type as SampleType};
use ffmpeg_next as ffmpeg;
use std::sync::{Once, OnceLock};

static INIT: Once = Once::new();

fn ensure_init() {
    INIT.call_once(|| {
        ffmpeg::init().expect("cannot initialize ffmpeg");
    });
}

const CHUNK_FRAMES: usize = 4096;

/// Duration multiplied by `1/tempo`, same rate and channels, pitch
/// preserved.
pub fn stretch_samples(
    samples: &[f32],
    sample_rate: u32,
    channels: u16,
    tempo: f64,
) -> Result<Vec<f32>, String> {
    let mut stretcher = Stretcher::new(sample_rate, channels, tempo)?;
    let mut out = Vec::new();
    stretcher.push(samples, &mut out)?;
    stretcher.finish(&mut out)?;
    Ok(out)
}

/// `stretch_samples` fed in pieces: the input never needs to be whole.
pub struct Stretcher {
    graph: ffmpeg::filter::Graph,
    sample_rate: u32,
    channels: usize,
    layout: ChannelLayout,
}

const FORMAT: SampleFormat = SampleFormat::F32(SampleType::Packed);

/// Whether this FFmpeg has the `rubberband` filter (Homebrew's build does not).
pub fn rubberband_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        ensure_init();
        ffmpeg::filter::find("rubberband").is_some()
    })
}

/// For the tests that need `rubberband`: true when they must be skipped
/// because it is missing. Never true under CI (`CI` set), so a missing
/// filter fails there.
#[doc(hidden)]
pub fn skip_without_rubberband() -> bool {
    static NOTE: Once = Once::new();
    if rubberband_available() || std::env::var_os("CI").is_some() {
        return false;
    }
    NOTE.call_once(|| {
        eprintln!("skipped: FFmpeg has no rubberband filter (see docs/BUILDING.md)");
    });
    true
}

impl Stretcher {
    pub fn new(sample_rate: u32, channels: u16, tempo: f64) -> Result<Self, String> {
        if tempo.is_nan() || tempo <= 0.0 {
            return Err(format!("invalid stretch tempo: {tempo}"));
        }
        ensure_init();
        let layout = ChannelLayout::default(channels as i32);
        let mut graph = ffmpeg::filter::Graph::new();
        let args = format!(
            "time_base=1/{sample_rate}:sample_rate={sample_rate}:sample_fmt={}:channel_layout=0x{:x}",
            FORMAT.name(),
            layout.bits()
        );
        graph
            .add(
                &ffmpeg::filter::find("abuffer").ok_or("abuffer filter not found")?,
                "in",
                &args,
            )
            .map_err(|e| e.to_string())?;
        graph
            .add(
                &ffmpeg::filter::find("abuffersink").ok_or("abuffersink filter not found")?,
                "out",
                "",
            )
            .map_err(|e| e.to_string())?;
        // `rubberband` can output planar even with packed input: `aformat`
        // forces packed, otherwise `drain_filtered` reads past the first
        // plane (panic with stereo audio).
        graph
            .output("in", 0)
            .and_then(|p| p.input("out", 0))
            .and_then(|p| {
                p.parse(&format!(
                    "rubberband=tempo={tempo},aformat=sample_fmts={}",
                    FORMAT.name()
                ))
            })
            .map_err(|e| e.to_string())?;
        graph.validate().map_err(|e| e.to_string())?;
        Ok(Self {
            graph,
            sample_rate,
            channels: channels.max(1) as usize,
            layout,
        })
    }

    /// Interleaved; appends to `out` what is stretched so far.
    pub fn push(&mut self, samples: &[f32], out: &mut Vec<f32>) -> Result<(), String> {
        let ch = self.channels;
        for chunk in samples.chunks(CHUNK_FRAMES * ch) {
            let n = chunk.len() / ch;
            let mut frame = ffmpeg::frame::Audio::new(FORMAT, n, self.layout);
            frame.set_rate(self.sample_rate);
            frame.plane_mut::<f32>(0)[..n * ch].copy_from_slice(&chunk[..n * ch]);
            self.graph
                .get("in")
                .ok_or("pad 'in' missing")?
                .source()
                .add(&frame)
                .map_err(|e| e.to_string())?;
            drain_filtered(&mut self.graph, ch, out)?;
        }
        Ok(())
    }

    /// Appends the rest.
    pub fn finish(mut self, out: &mut Vec<f32>) -> Result<(), String> {
        self.graph
            .get("in")
            .ok_or("pad 'in' missing")?
            .source()
            .flush()
            .map_err(|e| e.to_string())?;
        drain_filtered(&mut self.graph, self.channels, out)
    }
}

fn drain_filtered(
    graph: &mut ffmpeg::filter::Graph,
    channels: usize,
    out: &mut Vec<f32>,
) -> Result<(), String> {
    let mut filtered = ffmpeg::frame::Audio::empty();
    while graph
        .get("out")
        .ok_or("pad 'out' missing")?
        .sink()
        .frame(&mut filtered)
        .is_ok()
    {
        let n = filtered.samples();
        out.extend_from_slice(&filtered.plane::<f32>(0)[..n * channels]);
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/stretch.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/stretch_bench_window.rs"]
mod bench_window;
