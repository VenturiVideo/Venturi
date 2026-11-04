use super::*;

/// Stereo PCM in MKV has an "unknown" channel layout: it used to fail
/// with "Input changed".
#[test]
fn decode_audio_track_handles_unspecified_channel_layout() {
    let dir = crate::test_support::TempDir::new("vv-media-audio-test");
    let path = dir.join("pcm_unknown_layout.mkv");

    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=1",
            "-ac",
            "2",
            "-c:a",
            "pcm_s16le",
        ],
        &path,
    );

    let audio = decode_audio_track(&path, 0)
        .unwrap()
        .expect("audio expected");
    assert_eq!(audio.channels, 2);
    assert_eq!(audio.samples.len(), 48_000 * 2);
}

#[test]
fn decode_audio_track_reads_correct_length_and_is_not_silent() {
    let dir = crate::test_support::TempDir::new("vv-media-audio-test");
    let path = dir.join("sample.mp4");

    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=1",
            "-c:a",
            "aac",
        ],
        &path,
    );

    let audio = decode_audio_track(&path, 0)
        .unwrap()
        .expect("audio expected");
    assert_eq!(audio.sample_rate, 48000);
    assert_eq!(audio.channels, 1);

    let total_frames = audio.samples.len() / audio.channels as usize;
    // ~1s at 48kHz: the AAC encoder adds some priming/padding.
    assert!(
        (45_000..=52_000).contains(&total_frames),
        "total_frames={total_frames}"
    );

    // A 440Hz sine is not silent: the peak must be well above 0.
    let peak = audio.samples.iter().cloned().fold(0.0_f32, f32::max);
    assert!(peak > 0.1, "peak={peak}, expected a non-silent signal");
}

#[test]
fn streaming_decode_resamples_continuously_in_many_chunks() {
    let dir = crate::test_support::TempDir::new("vv-media-audio-test");
    let path = dir.join("streaming_44k.wav");
    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=44100:duration=2",
        ],
        &path,
    );

    let mut chunks = 0;
    let mut samples = Vec::new();
    let formats =
        decode_audio_streams_streaming(&path, &[0], Some(48_000), |_, channels, chunk| {
            assert_eq!(channels, 1);
            chunks += 1;
            samples.extend_from_slice(chunk);
            ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(formats, vec![Some((48_000, 1))]);
    assert!(chunks > 1, "chunks={chunks}");
    assert!(
        (samples.len() as i64 - 96_000).abs() < 100,
        "len={}",
        samples.len()
    );
    // A continuous sine: no jumps between samples near the chunk boundaries.
    let max_step = samples
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max);
    assert!(max_step < 0.1, "max_step={max_step}");
}

#[test]
fn streaming_decode_stops_when_the_callback_breaks() {
    let dir = crate::test_support::TempDir::new("vv-media-audio-test");
    let path = dir.join("streaming_break.wav");
    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=2",
        ],
        &path,
    );

    let mut chunks = 0;
    decode_audio_streams_streaming(&path, &[0], None, |_, _, _| {
        chunks += 1;
        ControlFlow::Break(())
    })
    .unwrap();
    assert_eq!(chunks, 1);
}

#[test]
fn streaming_decode_delivers_every_requested_stream_in_one_pass() {
    let dir = crate::test_support::TempDir::new("vv-media-audio-test");
    let path = dir.join("streaming_three.mkv");
    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=3",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=660:sample_rate=44100:duration=3",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=880:sample_rate=48000:duration=3",
            "-map",
            "0:a",
            "-map",
            "1:a",
            "-map",
            "2:a",
            "-c:a",
            "aac",
        ],
        &path,
    );

    let mut order = Vec::new();
    let mut lengths = [0usize; 3];
    let formats =
        decode_audio_streams_streaming(&path, &[2, 0, 5, 1], Some(48_000), |slot, _, chunk| {
            order.push(slot);
            let i = [0, 1, usize::MAX, 2][slot];
            lengths[i] += chunk.len();
            ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(
        formats,
        vec![
            Some((48_000, 1)),
            Some((48_000, 1)),
            None,
            Some((48_000, 1))
        ]
    );
    for len in lengths {
        assert!((len as i64 - 144_000).abs() < 3_000, "{lengths:?}");
    }
    // Interleaved: the first chunk of each stream arrives well before the end.
    let first_of = |slot| order.iter().position(|&s| s == slot).unwrap();
    assert!(
        [0, 1, 3].iter().all(|&s| first_of(s) < order.len() / 4),
        "{order:?}"
    );
}

/// A file with *two* audio streams (real case: stereo mix + separate
/// 5.1) must be able to decode one or the other based on
/// `stream_index`, not always "the best one" according to ffmpeg — here
/// they are told apart by sample_rate (44100 vs 48000) to check it
/// without spectral analysis.
#[test]
fn decode_audio_track_selects_the_requested_stream_index_not_just_the_best() {
    let dir = crate::test_support::TempDir::new("vv-media-audio-test");
    let path = dir.join("two_streams.mp4");

    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=44100:duration=1",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=880:sample_rate=48000:duration=1",
            "-map",
            "0:a",
            "-map",
            "1:a",
            "-c:a",
            "aac",
        ],
        &path,
    );

    let first = decode_audio_track(&path, 0)
        .unwrap()
        .expect("stream 0 expected");
    assert_eq!(first.sample_rate, 44100);

    let second = decode_audio_track(&path, 1)
        .unwrap()
        .expect("stream 1 expected");
    assert_eq!(second.sample_rate, 48000);

    assert!(
        decode_audio_track(&path, 2).unwrap().is_none(),
        "no audio stream at index 2"
    );
}

/// 20 s of seeded noise: unlike a tone, a shift of one sample shows.
const NOISE: &str = "anoisesrc=duration=20:color=white:amplitude=0.5:seed=7:sample_rate=48000";

/// A sweep, never periodic either: lossy codecs fill noise with noise of
/// their own (AAC's PNS), different after a seek.
const CHIRP: &str = "aevalsrc=0.5*sin(2*PI*(100+300*t)*t):duration=20:sample_rate=48000";

/// A generated test file; its directory is removed when the test ends.
struct SeekTestFile {
    _dir: crate::test_support::TempDir,
    path: std::path::PathBuf,
}

impl std::ops::Deref for SeekTestFile {
    type Target = std::path::Path;

    fn deref(&self) -> &std::path::Path {
        &self.path
    }
}

fn test_file(name: &str, source: &str, output_args: &[&str], with_video: bool) -> SeekTestFile {
    let dir = crate::test_support::TempDir::new("vv-media-seek-test");
    let file = SeekTestFile {
        path: dir.join(name),
        _dir: dir,
    };
    let mut args = vec!["-f", "lavfi", "-i", source];
    if with_video {
        args.extend([
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=20:size=320x240:rate=30",
            "-map",
            "1:v",
            "-map",
            "0:a",
            "-c:v",
            "libx264",
            "-g",
            "120",
        ]);
    }
    args.extend(["-ac", "2"]);
    args.extend(output_args);
    crate::test_support::ffmpeg(&args, &file.path);
    file
}

/// The frames `from..from + len` of stream 0 at 48 kHz stereo, by seeking.
fn seeked(path: &std::path::Path, from: u64, len: u64) -> Vec<f32> {
    let mut out = vec![f32::NAN; len as usize * 2];
    decode_audio_streams_from(path, &[0], 48_000, from, |_, channels, at, chunk| {
        assert_eq!(channels, 2);
        for (i, frame) in chunk.as_chunks::<2>().0.iter().enumerate() {
            let pos = at + i as u64;
            if (from..from + len).contains(&pos) {
                let o = (pos - from) as usize * 2;
                out[o..o + 2].copy_from_slice(frame);
            }
        }
        if at >= from + len {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })
    .unwrap();
    out
}

/// Stream 0 decoded from the start, each sample where `at` puts it: NaN
/// where nothing plays (a gap in the timestamps).
fn placed_full_decode(path: &std::path::Path) -> Vec<f32> {
    let mut full = Vec::new();
    decode_audio_streams_from(path, &[0], 48_000, 0, |_, _, at, chunk| {
        let start = at as usize * 2;
        if full.len() < start + chunk.len() {
            full.resize(start + chunk.len(), f32::NAN);
        }
        full[start..start + chunk.len()].copy_from_slice(chunk);
        ControlFlow::Continue(())
    })
    .unwrap();
    full
}

/// Inside the preroll, past a seek, and up to the end of the file.
const SEEK_OFFSETS: [u64; 4] = [300, 48_000 * 7 + 123, 48_000 * 13 + 7, 48_000 * 19 + 5];

fn assert_seek_matches_full_decode(path: &std::path::Path, tolerance: f32) {
    assert_seeks_match_full_decode(path, tolerance, &SEEK_OFFSETS);
}

fn assert_seeks_match_full_decode(path: &std::path::Path, tolerance: f32, offsets: &[u64]) {
    let full = placed_full_decode(path);
    for &from in offsets {
        let len = 48_000.min(full.len() as u64 / 2 - from);
        let got = seeked(path, from, len);
        let want = &full[from as usize * 2..(from + len) as usize * 2];
        let worst = worst_difference(&got, want);
        assert!(
            worst <= tolerance,
            "{}: from frame {from} the seeked decode differs by {worst}",
            path.display()
        );
    }
}

#[test]
fn a_seeked_decode_matches_the_full_decode_on_pcm() {
    let path = test_file("pcm.wav", NOISE, &["-c:a", "pcm_s16le"], false);
    assert_seek_matches_full_decode(&path, 0.0);
}

#[test]
fn a_seeked_decode_matches_the_full_decode_on_pcm_next_to_h264() {
    let path = test_file("obs.mkv", NOISE, &["-c:a", "pcm_s16le"], true);
    assert_seek_matches_full_decode(&path, 0.0);
}

#[test]
fn a_seeked_decode_matches_the_full_decode_on_flac() {
    let path = test_file("noise.flac", NOISE, &["-c:a", "flac"], false);
    assert_seek_matches_full_decode(&path, 0.0);
}

#[test]
fn a_seeked_decode_matches_the_full_decode_on_aac() {
    let path = test_file("aac.mp4", CHIRP, &["-c:a", "aac"], true);
    assert_seek_matches_full_decode(&path, 1e-4);
}

#[test]
fn a_seeked_decode_matches_the_full_decode_on_aac_in_mkv() {
    let path = test_file("aac.mkv", CHIRP, &["-c:a", "aac"], true);
    assert_seek_matches_full_decode(&path, 1e-4);
}

#[test]
fn a_seeked_decode_matches_the_full_decode_on_opus() {
    let path = test_file("opus.mkv", CHIRP, &["-c:a", "libopus"], true);
    assert_seek_matches_full_decode(&path, 1e-4);
}

#[test]
fn a_seeked_decode_matches_the_full_decode_on_mp3() {
    let path = test_file("chirp.mp3", CHIRP, &["-c:a", "libmp3lame"], false);
    assert_seek_matches_full_decode(&path, 1e-4);
}

fn chirp_at(rate: u32) -> String {
    format!("aevalsrc=0.5*sin(2*PI*(100+300*t)*t):duration=20:sample_rate={rate}")
}

/// Seeked onto an input frame that falls on an output frame, the resampler
/// is in phase with one fed from the start.
#[test]
fn a_seeked_resampled_decode_matches_the_full_decode() {
    for (name, rate, codec, tolerance) in [
        ("chirp44.wav", 44_100, "pcm_s16le", 0.0),
        ("chirp22.flac", 22_050, "flac", 0.0),
        ("chirp32.wav", 32_000, "pcm_s16le", 0.0),
        ("chirp96.wav", 96_000, "pcm_s16le", 0.0),
        // No channel layout in the file.
        ("chirp44.mkv", 44_100, "pcm_s16le", 0.0),
        ("chirp44.mp3", 44_100, "libmp3lame", 1e-4),
    ] {
        let path = test_file(name, &chirp_at(rate), &["-c:a", codec], false);
        assert_seek_matches_full_decode(&path, tolerance);
    }
    let path = test_file("chirp44.mp4", &chirp_at(44_100), &["-c:a", "aac"], true);
    assert_seeks_match_full_decode(&path, 1e-4, &SEEK_OFFSETS);
}

#[test]
fn a_resampled_file_is_read_from_near_the_frame_asked_not_from_its_start() {
    let path = test_file(
        "near44.wav",
        &chirp_at(44_100),
        &["-c:a", "pcm_s16le"],
        false,
    );
    let from = 48_000 * 13;
    let mut first = None;
    decode_audio_streams_from(&path, &[0], 48_000, from, |_, _, at, _| {
        first = Some(at);
        ControlFlow::Break(())
    })
    .unwrap();
    let first = first.unwrap();
    assert!(from - first <= 48_000, "first chunk at {first}");
}

/// 1 s missing from the timestamps at 5 s (a dropout, a cut capture): a
/// seek past it and a decode from the start both leave it silent.
#[test]
fn a_gap_in_the_timestamps_is_kept_by_a_decode_from_the_start_and_by_a_seek() {
    for (name, rate) in [("gap.mkv", 48_000), ("gap44.mkv", 44_100)] {
        let path = test_file(
            name,
            &chirp_at(rate),
            &[
                "-af",
                "asetpts='if(gte(T,5),PTS+SR,PTS)'",
                "-c:a",
                "pcm_s16le",
            ],
            false,
        );
        let full = placed_full_decode(&path);
        let silent = full[..full.len() / 2 * 2]
            .as_chunks::<2>()
            .0
            .iter()
            .filter(|frame| frame[0].is_nan())
            .count();
        assert!(
            (47_000..49_000).contains(&silent),
            "{name}: {silent} frames missing"
        );
        assert_seeks_match_full_decode(
            &path,
            0.0,
            &[
                48_000 * 3,
                48_000 * 4 + 24_000,
                48_000 * 6 + 600,
                48_000 * 13 + 7,
            ],
        );
    }
}

/// Each requested stream decoded from `from`, by seeking when `from > 0`,
/// as `(frames placed, NaN where nothing plays)` over `from..from + len`.
fn multi_decode(path: &std::path::Path, streams: &[usize], from: u64, len: u64) -> Vec<Vec<f32>> {
    let mut out = vec![vec![f32::NAN; len as usize * 2]; streams.len()];
    let mut done = vec![false; streams.len()];
    decode_audio_streams_from(path, streams, 48_000, from, |slot, channels, at, chunk| {
        assert_eq!(channels, 2);
        for (i, frame) in chunk.as_chunks::<2>().0.iter().enumerate() {
            let pos = at + i as u64;
            if (from..from + len).contains(&pos) {
                let o = (pos - from) as usize * 2;
                out[slot][o..o + 2].copy_from_slice(frame);
            }
        }
        done[slot] |= at >= from + len;
        if done.iter().all(|&d| d) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })
    .unwrap();
    out
}

fn worst_difference(got: &[f32], want: &[f32]) -> f32 {
    got.iter()
        .zip(want)
        .fold(0.0f32, |m, (g, w)| match (g.is_nan(), w.is_nan()) {
            (true, true) => m,
            (false, false) => m.max((g - w).abs()),
            _ => f32::INFINITY,
        })
}

/// Two audio streams of different codecs and lengths (20 s AAC at 48 kHz,
/// 14 s Opus from a different sweep), plus video, in one mkv.
#[test]
fn a_seeked_decode_of_several_streams_matches_the_full_decode_of_each() {
    let dir = crate::test_support::TempDir::new("vv-media-seek-multi");
    let path = dir.join("two_streams.mkv");
    let second = "aevalsrc=0.5*sin(2*PI*(200+500*t)*t):duration=14:sample_rate=48000";
    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            CHIRP,
            "-f",
            "lavfi",
            "-i",
            second,
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=20:size=320x240:rate=30",
            "-map",
            "2:v",
            "-map",
            "0:a",
            "-map",
            "1:a",
            "-c:v",
            "libx264",
            "-g",
            "120",
            "-ac",
            "2",
            "-c:a:0",
            "aac",
            "-c:a:1",
            "libopus",
        ],
        &path,
    );
    // Indices count audio streams only.
    let streams = [0, 1];
    let full_len = 48_000 * 14;
    let full = multi_decode(&path, &streams, 0, full_len);
    assert!(
        full.iter()
            .all(|f| f.iter().any(|v| *v != 0.0 && !v.is_nan()))
    );
    for from in [300, 48_000 * 7 + 123, 48_000 * 13 - 1_000] {
        let len = 24_000;
        let got = multi_decode(&path, &streams, from, len);
        for (slot, (g, f)) in got.iter().zip(&full).enumerate() {
            let want = &f[from as usize * 2..(from + len) as usize * 2];
            let worst = worst_difference(g, want);
            assert!(
                worst <= 1e-4,
                "stream slot {slot}: from frame {from} the seeked decode differs by {worst}"
            );
        }
    }
    // Past the end of the shorter stream the longer one still seeks.
    let from = 48_000 * 17 + 11;
    let long = multi_decode(&path, &streams, from, 24_000);
    let full_long = multi_decode(&path, &[0], 0, 48_000 * 20);
    let want = &full_long[0][from as usize * 2..(from + 24_000) as usize * 2];
    assert!(worst_difference(&long[0], want) <= 1e-4);
}

#[test]
fn a_seeked_decode_matches_the_full_decode_on_vbr_mp3() {
    let dir = crate::test_support::TempDir::new("vv-media-seek-vbr");
    let path = dir.join("chirp_vbr.mp3");
    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            CHIRP,
            "-ac",
            "2",
            "-c:a",
            "libmp3lame",
            "-q:a",
            "2",
        ],
        &path,
    );
    assert_seek_matches_full_decode(&path, 1e-4);
}
