use super::*;
use vv_core::ClipSource;
use vv_core::{Clip, Keyframed, Rgba, Track, TrackKind};

fn solid_color_clip(id: u64, start: FrameIdx, len: FrameIdx, color: Rgba) -> Clip {
    let mut clip = Clip::from_source_range(
        ClipId(id),
        ClipSource::SolidColor,
        0,
        len,
        start,
        vv_core::Rational::one(),
    );
    clip.effects.color = Some(Keyframed::constant(color));
    clip
}

fn timeline_with(tracks: Vec<Track>) -> Timeline {
    Timeline {
        name: "t".into(),
        fps: vv_core::Rational::new(25, 1),
        resolution: (4, 2),
        tracks,
        markers: Vec::new(),
        master: Default::default(),
    }
}

// BT.709 limited range, like `Compositor::render_layers_i420`.
const BLACK_I420: [u8; 3] = [16, 128, 128];
const RED_I420: [u8; 3] = [63, 102, 240];
const BLUE_I420: [u8; 3] = [32, 240, 118];

fn solid_i420(width: usize, height: usize, [y, u, v]: [u8; 3]) -> Vec<u8> {
    let chroma = width.div_ceil(2) * height.div_ceil(2);
    let mut data = vec![y; width * height];
    data.extend(std::iter::repeat_n(u, chroma));
    data.extend(std::iter::repeat_n(v, chroma));
    data
}

/// The I420 conversion dithers: a solid color off an 8-bit level comes out
/// one level up or down.
fn assert_i420_near(got: &[u8], expected: &[u8], context: &str) {
    assert_eq!(got.len(), expected.len(), "{context}");
    assert!(
        got.iter().zip(expected).all(|(a, b)| a.abs_diff(*b) <= 1),
        "{context}: {got:?} vs {expected:?}"
    );
}

fn red() -> Rgba {
    Rgba {
        r: 1.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    }
}

fn blue() -> Rgba {
    Rgba {
        r: 0.0,
        g: 0.0,
        b: 1.0,
        a: 1.0,
    }
}

#[test]
fn render_video_frame_returns_black_in_a_gap() {
    let project = Project::default();
    let tl = timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![solid_color_clip(1, 10, 5, red())],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]);
    let compositor = vv_render::Compositor::new_headless();
    let mut active = StreamingFrameProvider::default();
    let frame = render_video_frame(&project, &tl, &compositor, &mut active, 0, (2, 2)).unwrap();
    assert_i420_near(&frame, &solid_i420(2, 2, BLACK_I420), "frame");
}

#[test]
fn render_video_frame_reads_solid_color_at_the_clips_source_frame() {
    let project = Project::default();
    let tl = timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![solid_color_clip(1, 10, 5, red())],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]);
    let compositor = vv_render::Compositor::new_headless();
    let mut active = StreamingFrameProvider::default();
    let frame = render_video_frame(&project, &tl, &compositor, &mut active, 12, (2, 2)).unwrap();
    assert_i420_near(&frame, &solid_i420(2, 2, RED_I420), "frame");
}

/// A video clip inside the nested timeline of a compound clip must
/// show up in the exported frame, at the position of the compound clip
/// in the outer timeline — not before/after, and not at the one it
/// would have in its nested timeline.
#[test]
fn render_video_frame_recurses_into_a_compound_clips_nested_timeline() {
    let mut project = Project::default();
    let nested_id = project.timelines.insert(Timeline {
        name: "Nested".into(),
        fps: vv_core::Rational::new(25, 1),
        resolution: (2, 2),
        tracks: vec![Track {
            kind: TrackKind::Video,
            clips: vec![solid_color_clip(1, 0, 10, red())],
            muted: false,
            solo: false,
            locked: false,
            crossings: Vec::new(),
            mix: Default::default(),
            armed: Default::default(),
        }],
        markers: Vec::new(),
        master: Default::default(),
    });
    let compound_media = project.media_pool.insert(vv_core::MediaItem {
        path: "Compound Clip 1".into(),
        meta: vv_core::MediaMeta {
            duration_frames: 10,
            fps: vv_core::Rational::new(25, 1),
            width: 2,
            height: 2,
            has_video: true,
            has_audio: false,
            sample_rate: 0,
            channels: 0,
            audio_streams: 0,
            file: Default::default(),
        },
        content_hash: 1,
        compound: Some(nested_id),
        folder: None,
    });
    let compound_clip = Clip::from_source_range(
        ClipId(2),
        ClipSource::Media(compound_media),
        0,
        10,
        5,
        vv_core::Rational::one(),
    );
    let tl = timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![compound_clip],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]);
    let compositor = vv_render::Compositor::new_headless();
    let mut provider = StreamingFrameProvider::default();

    let before = render_video_frame(&project, &tl, &compositor, &mut provider, 0, (2, 2)).unwrap();
    assert_i420_near(
        &before,
        &solid_i420(2, 2, BLACK_I420),
        "before the compound clip: empty",
    );

    let during = render_video_frame(&project, &tl, &compositor, &mut provider, 7, (2, 2)).unwrap();
    assert_i420_near(
        &during,
        &solid_i420(2, 2, RED_I420),
        "inside: the content of the nested timeline",
    );
}

/// Bug reported by the user: a compound clip is a timeline like any
/// other, so where its nested timeline has nothing to show it must
/// stay transparent and let the track below show through —
/// not cover it with black.
#[test]
fn render_video_frame_lets_the_track_below_show_through_the_compound_clips_empty_area() {
    let mut project = Project::default();
    let mut nested_clip = solid_color_clip(1, 0, 10, red());
    // Cuts away the right half (crop in timeline pixels, nested 4x2).
    nested_clip.effects.transform = vv_core::TransformTracks::constant(vv_core::Transform {
        crop: [0.0, 0.0, 2.0, 0.0],
        ..Default::default()
    });
    let nested_id = project.timelines.insert(Timeline {
        name: "Nested".into(),
        fps: vv_core::Rational::new(25, 1),
        resolution: (4, 2),
        tracks: vec![Track {
            kind: TrackKind::Video,
            clips: vec![nested_clip],
            muted: false,
            solo: false,
            locked: false,
            crossings: Vec::new(),
            mix: Default::default(),
            armed: Default::default(),
        }],
        markers: Vec::new(),
        master: Default::default(),
    });
    let compound_media = project.media_pool.insert(vv_core::MediaItem {
        path: "Compound Clip 1".into(),
        meta: vv_core::MediaMeta {
            duration_frames: 10,
            fps: vv_core::Rational::new(25, 1),
            width: 4,
            height: 2,
            has_video: true,
            has_audio: false,
            sample_rate: 0,
            channels: 0,
            audio_streams: 0,
            file: Default::default(),
        },
        content_hash: 1,
        compound: Some(nested_id),
        folder: None,
    });
    let compound_clip = Clip::from_source_range(
        ClipId(2),
        ClipSource::Media(compound_media),
        0,
        10,
        0,
        vv_core::Rational::one(),
    );
    let tl = timeline_with(vec![
        Track {
            kind: TrackKind::Video,
            clips: vec![solid_color_clip(3, 0, 10, blue())],
            muted: false,
            solo: false,
            locked: false,
            crossings: Vec::new(),
            mix: Default::default(),
            armed: Default::default(),
        },
        Track {
            kind: TrackKind::Video,
            clips: vec![compound_clip],
            muted: false,
            solo: false,
            locked: false,
            crossings: Vec::new(),
            mix: Default::default(),
            armed: Default::default(),
        },
    ]);
    let compositor = vv_render::Compositor::new_headless();
    let mut provider = StreamingFrameProvider::default();

    let frame = render_video_frame(&project, &tl, &compositor, &mut provider, 0, (4, 2)).unwrap();
    // Y plane, one byte per pixel: left covered by the red of the
    // compound clip, right uncovered (the blue below must show).
    assert_i420_near(
        &frame[0..1],
        &RED_I420[..1],
        "left: the red of the compound clip",
    );
    assert_i420_near(
        &frame[3..4],
        &BLUE_I420[..1],
        "right: the blue of the track below, not black",
    );
}

/// A PNG with transparency imported into the pool must let the track
/// below show through where it is transparent, not cover it: its alpha must
/// be preserved from decode all the way to compositing.
#[test]
fn render_video_frame_lets_the_track_below_show_through_a_transparent_png() {
    let dir = std::env::temp_dir().join("vv-app-export-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("half_transparent.png");
    // Left half opaque red, right half fully transparent.
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=red:size=4x2:d=1",
            "-vf",
            "format=rgba,geq=r='255':g='0':b='0':a='if(lt(X,2),255,0)'",
            "-frames:v",
            "1",
            "-update",
            "1",
        ],
        &path,
    );

    let mut project = Project::default();
    let meta = vv_media::probe::probe_image(&path).expect("PNG probe failed");
    let png_media = project.media_pool.insert(vv_core::MediaItem {
        path: path.clone(),
        meta,
        content_hash: 1,
        compound: None,
        folder: None,
    });
    let png_clip = Clip::from_source_range(
        ClipId(2),
        ClipSource::Media(png_media),
        0,
        10,
        0,
        vv_core::Rational::one(),
    );
    let tl = timeline_with(vec![
        Track {
            kind: TrackKind::Video,
            clips: vec![solid_color_clip(3, 0, 10, blue())],
            muted: false,
            solo: false,
            locked: false,
            crossings: Vec::new(),
            mix: Default::default(),
            armed: Default::default(),
        },
        Track {
            kind: TrackKind::Video,
            clips: vec![png_clip],
            muted: false,
            solo: false,
            locked: false,
            crossings: Vec::new(),
            mix: Default::default(),
            armed: Default::default(),
        },
    ]);
    let compositor = vv_render::Compositor::new_headless();
    let mut provider = StreamingFrameProvider::default();

    let frame = render_video_frame(&project, &tl, &compositor, &mut provider, 0, (4, 2)).unwrap();
    assert!(
        (frame[0] as i16 - RED_I420[0] as i16).abs() <= 4,
        "left: the opaque red of the PNG, not the blue ({})",
        frame[0]
    );
    assert!(
        (frame[3] as i16 - BLUE_I420[0] as i16).abs() <= 4,
        "right: transparent, the blue below must show ({})",
        frame[3]
    );
}

#[test]
fn render_video_frame_applies_the_transform_to_a_solid_color_clip() {
    let project = Project::default();
    let mut clip = solid_color_clip(1, 0, 5, red());
    // Crop in timeline pixels (4x2): away with the right half.
    clip.effects.transform = vv_core::TransformTracks::constant(vv_core::Transform {
        crop: [0.0, 0.0, 2.0, 0.0],
        ..Default::default()
    });
    let tl = timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![clip],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]);
    let compositor = vv_render::Compositor::new_headless();
    let mut active = StreamingFrameProvider::default();
    let frame = render_video_frame(&project, &tl, &compositor, &mut active, 0, (4, 2)).unwrap();
    // First row of the Y plane.
    assert_i420_near(&frame[0..=0], &RED_I420[..1], "pixel 0");
    assert_i420_near(&frame[3..=3], &BLACK_I420[..1], "pixel 3");
}

/// A media missing from the pool is an `Err`, not a black frame.
#[test]
fn render_video_frame_fails_loudly_when_the_clip_references_a_missing_media() {
    let missing_media_id = {
        let mut other_project = Project::default();
        other_project.media_pool.insert(vv_core::MediaItem {
            path: "dummy.mp4".into(),
            meta: vv_core::MediaMeta {
                duration_frames: 0,
                fps: vv_core::Rational::new(25, 1),
                width: 0,
                height: 0,
                has_video: true,
                has_audio: false,
                sample_rate: 0,
                channels: 0,
                audio_streams: 0,
                file: Default::default(),
            },
            content_hash: 0,
            compound: None,
            folder: None,
        })
    };
    let project = Project::default();
    let tl = timeline_with(vec![Track {
        kind: TrackKind::Video,
        clips: vec![Clip::from_source_range(
            ClipId(1),
            ClipSource::Media(missing_media_id),
            0,
            10,
            0,
            vv_core::Rational::one(),
        )],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]);
    let compositor = vv_render::Compositor::new_headless();
    let mut provider = StreamingFrameProvider::default();
    let err = render_video_frame(&project, &tl, &compositor, &mut provider, 0, (2, 2))
        .expect_err("a media missing from the pool must fail, not produce a black frame");
    assert_eq!(err, ExportError::MediaNotFound);
}

/// Where the top track has no clip, the one below shows.
#[test]
fn render_video_frame_prefers_the_topmost_video_track() {
    let project = Project::default();
    let tl = timeline_with(vec![
        Track {
            kind: TrackKind::Video,
            clips: vec![solid_color_clip(1, 0, 30, red())],
            muted: false,
            solo: false,
            locked: false,
            crossings: Vec::new(),
            mix: Default::default(),
            armed: Default::default(),
        },
        Track {
            kind: TrackKind::Video,
            clips: vec![solid_color_clip(2, 10, 10, blue())],
            muted: false,
            solo: false,
            locked: false,
            crossings: Vec::new(),
            mix: Default::default(),
            armed: Default::default(),
        },
    ]);
    let compositor = vv_render::Compositor::new_headless();
    let mut provider = StreamingFrameProvider::default();

    let below = render_video_frame(&project, &tl, &compositor, &mut provider, 5, (2, 2)).unwrap();
    assert_i420_near(
        &below,
        &solid_i420(2, 2, RED_I420),
        "below the top track: the bottom one shows",
    );

    let above = render_video_frame(&project, &tl, &compositor, &mut provider, 15, (2, 2)).unwrap();
    assert_i420_near(
        &above,
        &solid_i420(2, 2, BLUE_I420),
        "the top track has a clip here: it wins",
    );
}

#[test]
fn mix_audio_track_is_silence_when_no_audio_track_has_clips() {
    let project = Project::default();
    let tl = timeline_with(vec![
        Track {
            kind: TrackKind::Video,
            clips: vec![solid_color_clip(1, 0, 25, red())],
            muted: false,
            solo: false,
            locked: false,
            crossings: Vec::new(),
            mix: Default::default(),
            armed: Default::default(),
        },
        Track {
            kind: TrackKind::Audio,
            clips: vec![],
            muted: false,
            solo: false,
            locked: false,
            crossings: Vec::new(),
            mix: Default::default(),
            armed: Default::default(),
        },
    ]);
    let mixed = mix_audio_track(&project, &tl, 0..25).unwrap();
    assert_eq!(
        mixed.len(),
        (PROJECT_SAMPLE_RATE as usize) * PROJECT_CHANNELS as usize
    );
    assert!(mixed.iter().all(|&s| s == 0.0));
}

/// A real audio file inside the nested timeline of a compound clip
/// must reach the export mix, at the position of the compound
/// clip in the outer timeline — not at the one it would have in its
/// nested timeline.
#[test]
fn mix_audio_track_recurses_into_a_compound_clips_nested_timeline() {
    let dir = std::env::temp_dir().join("vv-app-export-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("compound_audio_source.wav");
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=1",
        ],
        &path,
    );

    let mut project = Project::default();
    let real_media = project.media_pool.insert(vv_core::MediaItem {
        path: path.clone(),
        meta: vv_core::MediaMeta {
            duration_frames: 25,
            fps: vv_core::Rational::new(25, 1),
            width: 0,
            height: 0,
            has_video: false,
            has_audio: true,
            sample_rate: 48_000,
            channels: 1,
            audio_streams: 1,
            file: Default::default(),
        },
        content_hash: 1,
        compound: None,
        folder: None,
    });
    let real_clip = Clip::from_source_range(
        ClipId(100),
        ClipSource::Media(real_media),
        0,
        25,
        0,
        vv_core::Rational::one(),
    );
    let nested_id = project.timelines.insert(Timeline {
        name: "Nested".into(),
        fps: vv_core::Rational::new(25, 1),
        resolution: (1, 1),
        tracks: vec![Track {
            kind: TrackKind::Audio,
            clips: vec![real_clip],
            muted: false,
            solo: false,
            locked: false,
            crossings: Vec::new(),
            mix: Default::default(),
            armed: Default::default(),
        }],
        markers: Vec::new(),
        master: Default::default(),
    });
    let compound_media = project.media_pool.insert(vv_core::MediaItem {
        path: "Compound Clip 1".into(),
        meta: vv_core::MediaMeta {
            duration_frames: 25,
            fps: vv_core::Rational::new(25, 1),
            width: 0,
            height: 0,
            has_video: false,
            has_audio: true,
            sample_rate: 48_000,
            channels: 1,
            audio_streams: 1,
            file: Default::default(),
        },
        content_hash: 2,
        compound: Some(nested_id),
        folder: None,
    });
    // At 25 (1s after the start): silence before, sine wave during.
    let compound_clip = Clip::from_source_range(
        ClipId(1),
        ClipSource::Media(compound_media),
        0,
        25,
        25,
        vv_core::Rational::one(),
    );
    let tl = timeline_with(vec![Track {
        kind: TrackKind::Audio,
        clips: vec![compound_clip],
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    }]);

    let mixed = mix_audio_track(&project, &tl, 0..50).unwrap();
    let per_second = PROJECT_SAMPLE_RATE as usize * PROJECT_CHANNELS as usize;
    assert!(
        mixed[..per_second].iter().all(|&s| s == 0.0),
        "silence before the compound clip"
    );
    assert!(
        mixed[per_second..].iter().any(|&s| s.abs() > 0.01),
        "the nested timeline content reaches the mix at the compound clip position"
    );
}

/// Regression for a user report of audio running ahead of the
/// video on export (perceived with mpv, 2-3 frames): a black->white
/// flash (`ClipSource::SolidColor`, no video encode/decode
/// involved in its position) and a beep (a real audio clip) that
/// start at the same timeline frame, at a fractional NTSC fps
/// (30000/1001) on purpose so that `PROJECT_SAMPLE_RATE / fps` is not an
/// integer (at 24fps it is exactly 2000, which would hide a
/// rounding bug) — the real case isolated from the report
/// was already at an integer fps, and here it comes out perfectly in
/// sync anyway: if this test breaks, the bug is in the
/// frame<->sample rounding of `export.rs`/`encode.rs`/`vv_audio::mixer`, not
/// in the original cause of the report (probably on the
/// player side, not in this export).
#[test]
fn export_keeps_video_and_audio_frame_accurate_at_a_fractional_ntsc_fps() {
    let dir = std::env::temp_dir().join("vv-app-diag-avsync-frac");
    std::fs::create_dir_all(&dir).unwrap();
    let beep_path = dir.join("beep.wav");
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=1000:sample_rate=48000:duration=1",
        ],
        &beep_path,
    );

    let mut project = Project::default();
    let beep_media = project.media_pool.insert(vv_core::MediaItem {
        path: beep_path,
        meta: vv_core::MediaMeta {
            duration_frames: 30,
            fps: vv_core::Rational::new(30000, 1001),
            width: 0,
            height: 0,
            has_video: false,
            has_audio: true,
            sample_rate: 48_000,
            channels: 1,
            audio_streams: 1,
            file: Default::default(),
        },
        content_hash: 1,
        compound: None,
        folder: None,
    });
    const FLASH_FRAME: FrameIdx = 20;
    let beep_clip = Clip::from_source_range(
        ClipId(1),
        ClipSource::Media(beep_media),
        0,
        25,
        FLASH_FRAME,
        vv_core::Rational::one(),
    );
    let tl = Timeline {
        name: "diag".into(),
        fps: vv_core::Rational::new(30000, 1001),
        resolution: (64, 48),
        tracks: vec![
            Track {
                kind: TrackKind::Video,
                clips: vec![
                    solid_color_clip(10, 0, FLASH_FRAME, black()),
                    solid_color_clip(11, FLASH_FRAME, 15, white()),
                ],
                muted: false,
                solo: false,
                locked: false,
                crossings: Vec::new(),
                mix: Default::default(),
                armed: Default::default(),
            },
            Track {
                kind: TrackKind::Audio,
                clips: vec![beep_clip],
                muted: false,
                solo: false,
                locked: false,
                crossings: Vec::new(),
                mix: Default::default(),
                armed: Default::default(),
            },
        ],
        markers: Vec::new(),
        master: Default::default(),
    };
    let timeline_id = project.timelines.insert(tl);

    let output_path = dir.join("out.mp4");
    let progress = Mutex::new(ExportProgress::default());
    let cancel = AtomicBool::new(false);
    let total = project.timelines[timeline_id].total_frames();
    let settings = ExportSettings::new(output_path.clone());
    export_timeline(
        &project,
        timeline_id,
        &settings,
        0..total,
        &progress,
        &cancel,
    )
    .expect("export failed");

    let fps = 30000.0_f64 / 1001.0;

    let mut decoder = vv_media::Decoder::open(&output_path).unwrap();
    let mut white_frame_idx = None;
    let mut idx = 0i64;
    while let Some((_, frame)) = decoder.next_frame().unwrap() {
        let avg_y = frame.y.iter().map(|&b| b as u64).sum::<u64>() / frame.y.len() as u64;
        if avg_y > 128 && white_frame_idx.is_none() {
            white_frame_idx = Some(idx);
        }
        idx += 1;
    }
    let white_frame_idx = white_frame_idx.expect("no white frame found in the export");

    let audio = vv_media::decode_audio_track(&output_path, 0)
        .unwrap()
        .expect("audio expected");
    let mut onset_sample = None;
    for (i, &s) in audio.samples.iter().enumerate() {
        if s.abs() > 0.05 {
            onset_sample = Some(i as u64 / audio.channels as u64);
            break;
        }
    }
    let onset_sample = onset_sample.expect("no audio onset found in the export");
    let onset_secs = onset_sample as f64 / audio.sample_rate as f64;
    let onset_frame = (onset_secs * fps).floor() as i64;

    assert_eq!(
        white_frame_idx, FLASH_FRAME,
        "the video flash is not at the expected frame"
    );
    assert_eq!(
        onset_frame,
        FLASH_FRAME,
        "the audio onset ({onset_secs:.6}s) falls in frame {onset_frame} instead of the video flash frame {FLASH_FRAME}: {} frame offset",
        FLASH_FRAME - onset_frame
    );
}

fn black() -> Rgba {
    Rgba {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    }
}

fn white() -> Rgba {
    Rgba {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 1.0,
    }
}

#[test]
fn preferred_settings_pick_the_faster_encoders_only_when_available() {
    let settings = ExportSettings::preferred(PathBuf::from("out.mp4"));
    let nvenc = vv_media::VideoCodec::Nvenc.is_available();
    assert_eq!(settings.video.codec == vv_media::VideoCodec::Nvenc, nvenc);
    assert_eq!(settings.video.preset, settings.video.codec.default_preset());
    let fdk = vv_media::AudioCodec::FdkAac.is_available();
    let audio = settings.audio.expect("audio included by default");
    assert_eq!(audio.codec == vv_media::AudioCodec::FdkAac, fdk);
}

/// 2 s of a 440 Hz sine at 200%: 1 s on the timeline, at 440 Hz with pitch
/// correction and at 880 Hz without.
#[test]
fn mix_audio_track_plays_a_faster_clip_with_or_without_its_pitch() {
    let dir = std::env::temp_dir().join("vv-app-export-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("speed_source.wav");
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=2",
        ],
        &path,
    );
    let mut project = Project::default();
    let media = project.media_pool.insert(vv_core::MediaItem {
        path,
        meta: vv_core::MediaMeta {
            duration_frames: 50,
            fps: vv_core::Rational::new(25, 1),
            width: 0,
            height: 0,
            has_video: false,
            has_audio: true,
            sample_rate: 48_000,
            channels: 1,
            audio_streams: 1,
            file: Default::default(),
        },
        content_hash: 1,
        compound: None,
        folder: None,
    });
    let frequency = |pitch_correction: bool| {
        let mut clip = Clip::new(
            ClipId(1),
            ClipSource::Media(media),
            0,
            0,
            25,
            vv_core::Rational::one(),
            vv_core::Rational::new(2, 1),
        );
        clip.pitch_correction = pitch_correction;
        let tl = timeline_with(vec![Track {
            kind: TrackKind::Audio,
            clips: vec![clip],
            muted: false,
            solo: false,
            locked: false,
            crossings: Vec::new(),
            mix: Default::default(),
            armed: Default::default(),
        }]);
        let mixed = mix_audio_track(&project, &tl, 0..25).unwrap();
        // First channel, the middle half second: away from the stretch's edges.
        let mono: Vec<f32> = mixed
            .iter()
            .step_by(PROJECT_CHANNELS as usize)
            .copied()
            .collect();
        let window = &mono[12_000..36_000];
        let crossings = window
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        crossings as f64 / 2.0 / 0.5
    };
    let corrected = frequency(true);
    let varispeed = frequency(false);
    assert!(
        (corrected - 440.0).abs() < 20.0,
        "pitch kept: {corrected} Hz"
    );
    assert!(
        (varispeed - 880.0).abs() < 20.0,
        "pitch follows: {varispeed} Hz"
    );
}

/// A range decodes only the frames it reads, resampled like the whole file.
#[test]
fn mix_audio_track_of_a_range_is_the_same_part_of_the_whole_mix() {
    let dir = std::env::temp_dir().join("vv-app-export-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("range_source.wav");
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "aevalsrc=sin(40*t*t):sample_rate=44100:duration=4",
        ],
        &path,
    );
    let mut project = Project::default();
    let media = project.media_pool.insert(vv_core::MediaItem {
        path,
        meta: vv_core::MediaMeta {
            duration_frames: 100,
            fps: vv_core::Rational::new(25, 1),
            width: 0,
            height: 0,
            has_video: false,
            has_audio: true,
            sample_rate: 44_100,
            channels: 1,
            audio_streams: 1,
            file: Default::default(),
        },
        content_hash: 1,
        compound: None,
        folder: None,
    });
    let clip = |id, source_in, start| {
        Clip::from_source_range(
            ClipId(id),
            ClipSource::Media(media),
            source_in,
            source_in + 40,
            start,
            vv_core::Rational::one(),
        )
    };
    let faster = Clip::new(
        ClipId(2),
        ClipSource::Media(media),
        10,
        20,
        30,
        vv_core::Rational::one(),
        vv_core::Rational::new(3, 2),
    );
    let track = |clips| Track {
        kind: TrackKind::Audio,
        clips,
        muted: false,
        solo: false,
        locked: false,
        crossings: Vec::new(),
        mix: Default::default(),
        armed: Default::default(),
    };
    let tl = timeline_with(vec![track(vec![clip(1, 30, 5)]), track(vec![faster])]);

    let whole = mix_audio_track(&project, &tl, 0..75).unwrap();
    let part = mix_audio_track(&project, &tl, 30..40).unwrap();
    let per_frame = (PROJECT_SAMPLE_RATE / 25) as usize * PROJECT_CHANNELS as usize;
    assert_eq!(part, whole[30 * per_frame..40 * per_frame]);
    assert!(part.iter().any(|&s| s.abs() > 0.1));

    let mut wanted = WantedFrames::default();
    let samples = 30 * 1920..40 * 1920;
    MixSnapshot::from_timeline_range(&project, &tl, 48_000, 2, &mut wanted, samples);
    let windows = &wanted.0.values().next().unwrap();
    assert!(
        windows.iter().all(|(_, frames)| frames.end < 4 * 48_000),
        "{windows:?}"
    );
}
