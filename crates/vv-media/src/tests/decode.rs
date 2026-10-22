use super::*;

/// U and V as planes, whatever the layout of the frame.
fn planar_chroma(frame: &FrameYuv420) -> (Vec<u8>, Vec<u8>) {
    let (w, h) = (frame.chroma_width as usize, frame.chroma_height as usize);
    (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| frame.chroma_at(x, y))
        .unzip()
}

fn decode_first_frame(path: &Path) -> Result<Arc<FrameYuv420>, crate::MediaError> {
    let mut decoder = Decoder::open(path)?;
    decoder
        .next_frame()?
        .map(|(_, frame)| frame)
        .ok_or_else(|| crate::MediaError::NoStream(path.display().to_string()))
}

#[test]
fn guess_matrix_uses_the_signaled_space_when_present() {
    assert_eq!(guess_matrix(color::Space::BT709, 240), ColorMatrix::Bt709);
    assert_eq!(
        guess_matrix(color::Space::BT2020NCL, 240),
        ColorMatrix::Bt2020
    );
    assert_eq!(
        guess_matrix(color::Space::SMPTE170M, 1080),
        ColorMatrix::Bt601,
        "an explicitly signalled SD matrix wins over the resolution heuristic"
    );
}

#[test]
fn guess_matrix_falls_back_to_a_resolution_heuristic_when_unspecified() {
    assert_eq!(
        guess_matrix(color::Space::Unspecified, 240),
        ColorMatrix::Bt601,
        "SD unsignalled: BT.601"
    );
    assert_eq!(
        guess_matrix(color::Space::Unspecified, 1080),
        ColorMatrix::Bt709,
        "HD unsignalled: BT.709"
    );
}

#[test]
fn guess_matrix_never_guesses_bt2020_from_the_heuristic() {
    // BT.2020 is too specific to be guessed: even at UHD resolutions,
    // without an explicit signal it stays on BT.709, never
    // BT.2020.
    assert_eq!(
        guess_matrix(color::Space::Unspecified, 2160),
        ColorMatrix::Bt709
    );
}

fn make_test_clip(
    name: &str,
    duration_secs: u32,
) -> (crate::test_support::TempDir, std::path::PathBuf) {
    let dir = crate::test_support::TempDir::new("vv-media-decode-test");
    let path = dir.join(name);

    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc=size=320x240:rate=25:duration={duration_secs}"),
            "-c:v",
            "libx264",
            "-g",
            "10", // short GOP: more keyframes to test the seek
            "-pix_fmt",
            "yuv420p",
        ],
        &path,
    );
    (dir, path)
}

#[test]
fn fitted_size_shrinks_only_what_does_not_fit_and_keeps_it_even() {
    assert_eq!(fitted_size((1920, 1080), None), (1920, 1080));
    assert_eq!(fitted_size((1920, 1080), Some((1920, 1080))), (1920, 1080));
    assert_eq!(fitted_size((1920, 1080), Some((480, 270))), (480, 270));
    assert_eq!(fitted_size((1920, 1080), Some((960, 2000))), (960, 540));
    assert_eq!(fitted_size((1280, 720), Some((480, 270))), (480, 270));
    assert_eq!(fitted_size((322, 182), Some((161, 91))), (162, 92));
}

#[test]
fn a_max_size_scales_the_frames_down_with_the_same_picture() {
    let (_dir, path) = make_test_clip("max-size.mp4", 1);
    let full = decode_first_frame(&path).unwrap();
    let mut decoder = Decoder::open(&path).unwrap();
    decoder.set_max_size(Some((160, 120)));
    let (_, small) = decoder.next_frame().unwrap().unwrap();

    assert_eq!((small.width, small.height), (160, 120));
    assert_eq!((small.chroma_width, small.chroma_height), (80, 60));
    assert_eq!(small.y.len(), 160 * 120);
    let mean = |y: &[u8]| y.iter().map(|&v| v as u64).sum::<u64>() / y.len() as u64;
    assert!(
        mean(&full.y).abs_diff(mean(&small.y)) <= 2,
        "same picture, only smaller"
    );
}

/// Phone screen capture: frames only when the picture changes, so the
/// pts leave long holes. 30 fps declared, 6 frames in 3.1 s.
fn make_vfr_test_clip(name: &str) -> (crate::test_support::TempDir, std::path::PathBuf) {
    let dir = crate::test_support::TempDir::new("vv-media-decode-test");
    let path = dir.join(name);
    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x64:rate=30:duration=4",
            "-vf",
            "select='eq(n,0)+eq(n,10)+eq(n,50)+eq(n,90)+eq(n,91)+eq(n,92)'",
            "-fps_mode",
            "passthrough",
            "-c:v",
            "libx264",
            "-g",
            "1",
            "-pix_fmt",
            "yuv420p",
        ],
        &path,
    );
    (dir, path)
}

/// The cache demands every index of a range: on a VFR source the holes
/// between one pts and the next were never filled and the preview
/// waited forever for frames the stream does not contain.
#[test]
fn a_vfr_stream_is_decoded_as_a_contiguous_cfr_sequence() {
    let (_dir, path) = make_vfr_test_clip("vfr.mp4");
    let mut decoder = Decoder::open(&path).unwrap();
    let mut indices = Vec::new();
    while let Some((idx, _)) = decoder.next_frame().unwrap() {
        indices.push(idx);
    }
    assert_eq!(
        indices,
        (0..=92).collect::<Vec<_>>(),
        "expected the full CFR sequence from the 6 real frames"
    );
}

/// The holes are filled by sharing the held frame: a long still
/// stretch must not cost one full copy per CFR slot.
#[test]
fn the_frames_filling_a_hole_share_one_allocation() {
    let (_dir, path) = make_vfr_test_clip("vfr_shared.mp4");
    let mut decoder = Decoder::open(&path).unwrap();
    let mut frames = Vec::new();
    while let Some((_, frame)) = decoder.next_frame().unwrap() {
        frames.push(frame);
    }
    let distinct = frames
        .iter()
        .map(Arc::as_ptr)
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        distinct.len(),
        6,
        "expected the 6 allocations of the real frames"
    );
}

#[test]
fn a_vfr_stream_stays_contiguous_after_a_seek() {
    let (_dir, path) = make_vfr_test_clip("vfr_seek.mp4");
    let mut decoder = Decoder::open(&path).unwrap();
    decoder.seek_to_time(2.0).unwrap();
    let mut indices = Vec::new();
    while let Some((idx, _)) = decoder.next_frame().unwrap() {
        indices.push(idx);
    }
    assert!(
        indices.first().is_some_and(|&first| first <= 60),
        "the seek must land at 2 s or earlier: {:?}",
        indices.first()
    );
    assert!(
        indices.windows(2).all(|w| w[1] == w[0] + 1),
        "non-contiguous indices after the seek: {indices:?}"
    );
    assert_eq!(indices.last(), Some(&92));
}

fn make_test_clip_with_gop_and_bframes(
    name: &str,
    duration_secs: u32,
    gop: u32,
    bframes: u32,
) -> (crate::test_support::TempDir, std::path::PathBuf) {
    let dir = crate::test_support::TempDir::new("vv-media-decode-test");
    let path = dir.join(name);

    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc=size=320x240:rate=25:duration={duration_secs}"),
            "-c:v",
            "libx264",
            "-g",
            &gop.to_string(),
            "-keyint_min",
            &gop.to_string(),
            "-bf",
            &bframes.to_string(),
            "-pix_fmt",
            "yuv420p",
        ],
        &path,
    );
    (dir, path)
}

#[test]
#[ignore = "manual measurement, not a correctness assertion"]
fn bench_decode_forward_through_a_large_gop() {
    let dir = crate::test_support::TempDir::new("vv-media-decode-bench");
    let path = dir.join("large_gop.mp4");
    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=1920x1080:rate=25:duration=12",
            "-c:v",
            "libx264",
            "-preset",
            "medium",
            "-g",
            "250",
            "-keyint_min",
            "250",
            "-pix_fmt",
            "yuv420p",
        ],
        &path,
    );

    let mut decoder = Decoder::open(&path).expect("open failed");
    decoder.seek_to_time(0.0).expect("seek failed");
    let start = std::time::Instant::now();
    let mut count = 0;
    while count < 249 {
        match decoder.next_frame().expect("decode failed") {
            Some(_) => count += 1,
            None => break,
        }
    }
    eprintln!(
        "decoded {count} frames (1920x1080) in {:?} ({:.1} fps)",
        start.elapsed(),
        count as f64 / start.elapsed().as_secs_f64()
    );
}

#[test]
fn decode_first_frame_of_x264_reads_correct_dimensions_and_pixels() {
    let (_dir, path) = make_test_clip("sample.mp4", 1);

    let frame = decode_first_frame(&path).expect("decode failed");
    assert_eq!(frame.width, 320);
    assert_eq!(frame.height, 240);
    assert_eq!(frame.y.len(), 320 * 240, "dense Y plane, 1 byte/pixel");
    // 4:2:0: chroma planes at half resolution (rounded up,
    // exact here because 320x240 is already even).
    assert_eq!(frame.chroma_width, 160);
    assert_eq!(frame.chroma_height, 120);
    let (u, v) = planar_chroma(&frame);
    assert_eq!(u.len(), 160 * 120);
    assert_eq!(v.len(), 160 * 120);

    // The testsrc pattern is never uniform: if we find more than one
    // distinct value in the Y plane, the stride/format are correct.
    let distinct: std::collections::HashSet<u8> = frame.y.iter().step_by(37).copied().collect();
    assert!(
        distinct.len() > 5,
        "the decoded pixels look degenerate: {distinct:?}"
    );
}

/// A YUV420P source skips the scaler: its planes, odd sizes included, must
/// come out byte for byte as ffmpeg decodes them.
#[test]
fn a_yuv420p_source_is_copied_plane_by_plane_with_odd_dimensions() {
    let dir = crate::test_support::TempDir::new("vv-media-decode-test");
    let path = dir.join("odd_yuv420p.mkv");
    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=321x241:rate=25:duration=1",
            "-c:v",
            "ffv1",
            "-pix_fmt",
            "yuv420p",
        ],
        &path,
    );
    let reference_path = dir.join("odd_yuv420p.yuv");
    crate::test_support::ffmpeg(
        &[
            "-i",
            path.to_str().unwrap(),
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
        ],
        &reference_path,
    );

    let frame = decode_first_frame(&path).expect("decode failed");
    assert_eq!((frame.chroma_width, frame.chroma_height), (161, 121));
    let (u, v) = planar_chroma(&frame);
    let decoded = [frame.y.as_slice(), &u, &v].concat();
    assert!(decoded == std::fs::read(&reference_path).unwrap());
}

fn make_test_image(name: &str) -> (crate::test_support::TempDir, std::path::PathBuf) {
    let dir = crate::test_support::TempDir::new("vv-media-decode-test");
    let path = dir.join(name);

    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x240:rate=1:duration=1",
            "-frames:v",
            "1",
            "-update",
            "1",
        ],
        &path,
    );
    (dir, path)
}

/// A PNG with transparency must carry it all the way to the decoded frame:
/// scaled to YUV420P the alpha would disappear and the transparent areas
/// would end up opaque, in the color left underneath in the file.
#[test]
fn a_transparent_png_keeps_its_alpha_channel() {
    let dir = crate::test_support::TempDir::new("vv-media-decode-test");
    let path = dir.join("half_transparent.png");
    // Left half opaque, right half fully transparent.
    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=red:size=16x8:d=1",
            "-vf",
            "format=rgba,geq=r='255':g='0':b='0':a='if(lt(X,8),255,0)'",
            "-frames:v",
            "1",
            "-update",
            "1",
        ],
        &path,
    );

    let mut decoder = Decoder::open_image(&path).expect("image open failed");
    let (_, frame) = decoder.next_frame().unwrap().expect("frame expected");

    let alpha = frame
        .alpha
        .as_ref()
        .expect("a PNG with transparency must carry the alpha plane");
    assert_eq!(
        alpha.len(),
        (frame.width * frame.height) as usize,
        "alpha not subsampled"
    );
    let at = |x: u32, y: u32| alpha[(y * frame.width + x) as usize];
    assert_eq!(at(2, 4), 255, "left: opaque");
    assert_eq!(at(13, 4), 0, "right: transparent, not opaque");
}

/// An RGB frame (a PNG) always declares `color_range = JPEG`, but that
/// is the range of the RGB: the YUV sws derives from it is limited range, and
/// believing it full would wash out every imported image.
#[test]
fn a_png_is_reported_as_limited_range_because_that_is_what_the_scaler_produces() {
    let (_dir, path) = make_test_image("range.png");
    let mut decoder = Decoder::open_image(&path).expect("image open failed");
    let (_, frame) = decoder.next_frame().unwrap().expect("frame expected");

    assert!(!frame.full_range);
    assert!(
        frame.y.iter().all(|&y| (16..=235).contains(&y)),
        "values outside the limited range: not the declared range"
    );
}

/// A video without an alpha channel must not pay for a fourth, fully
/// opaque coverage plane: it costs cache memory and one texture upload
/// per frame.
#[test]
fn a_video_without_alpha_carries_no_alpha_plane() {
    let (_dir, path) = make_test_clip("no-alpha.mp4", 1);
    let mut decoder = Decoder::open(&path).expect("open failed");
    let (_, frame) = decoder.next_frame().unwrap().expect("frame expected");

    assert!(frame.alpha.is_none());
}

#[test]
fn open_image_decodes_correct_dimensions_and_pixels() {
    let (_dir, path) = make_test_image("still.png");
    let decoder = Decoder::open_image(&path).expect("image open failed");
    assert_eq!(decoder.width(), 320);
    assert_eq!(decoder.height(), 240);
}

/// An image has no "next frame": any requested `source_frame` (via
/// `seek_to_time`) or a sequential call to `next_frame` without a seek
/// must always return the same content, never `None` as a real video
/// would after the single available frame.
#[test]
fn open_image_returns_the_same_frame_for_any_requested_position() {
    let (_dir, path) = make_test_image("still_repeat.png");
    let mut decoder = Decoder::open_image(&path).expect("image open failed");

    decoder.seek_to_time(0.0).unwrap();
    let (idx0, frame0) = decoder.next_frame().unwrap().expect("frame expected");
    assert_eq!(idx0, 0);

    decoder.seek_to_time(120.0).unwrap();
    let (idx_far, frame_far) = decoder
        .next_frame()
        .unwrap()
        .expect("frame expected even far ahead in time");
    assert_eq!(
        idx_far,
        (120.0 * crate::probe::IMAGE_FPS.as_f64()).round() as FrameIdx
    );
    assert_eq!(
        frame_far.y, frame0.y,
        "the very same frame, whatever the position"
    );

    // Without a seek in between, next_frame keeps returning
    // something (never None) instead of behaving like a real EOF.
    let (idx_next, frame_next) = decoder
        .next_frame()
        .unwrap()
        .expect("never EOF for an image");
    assert!(idx_next > idx_far);
    assert_eq!(frame_next.y, frame0.y);
}

/// JPEGs decode into `yuvj420p`: the full range must be preserved, not
/// compressed to limited by the scaler.
#[test]
fn full_range_jpeg_keeps_its_luma_range() {
    let dir = crate::test_support::TempDir::new("vv-media-decode-test");
    let path = dir.join("white.jpg");
    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "color=white:size=64x64",
            "-frames:v",
            "1",
            "-update",
            "1",
        ],
        &path,
    );
    let mut decoder = Decoder::open_image(&path).expect("image open failed");
    let (_, frame) = decoder.next_frame().unwrap().expect("frame expected");
    assert!(frame.full_range);
    assert!(
        frame.y.iter().all(|&y| y >= 250),
        "compressed luma: {}",
        frame.y[0]
    );
}

#[test]
fn decode_first_frame_defaults_to_mpeg_limited_range_and_a_resolution_based_matrix() {
    // make_test_clip does not explicitly signal matrix/range (common
    // for generated/consumer content): 320x240 is below the 720p
    // threshold, it must fall back on BT.601 + limited range.
    let (_dir, path) = make_test_clip("colorspace.mp4", 1);
    let frame = decode_first_frame(&path).expect("decode failed");
    assert_eq!(frame.matrix, ColorMatrix::Bt601);
    assert!(
        !frame.full_range,
        "the default range for video must be limited (MPEG), not full"
    );
}

#[test]
fn decoder_sequential_frames_have_increasing_index() {
    let (_dir, path) = make_test_clip("sequential.mp4", 1);
    let mut decoder = Decoder::open(&path).unwrap();

    let mut last_idx = -1;
    let mut count = 0;
    while let Some((idx, _)) = decoder.next_frame().unwrap() {
        assert!(idx > last_idx, "frame indices must increase");
        last_idx = idx;
        count += 1;
    }
    // ~25fps for 1s: a few frames of tolerance on the last GOP.
    assert!((20..=30).contains(&count), "count={count}");
}

#[test]
fn decoder_seek_lands_near_target_and_decodes_forward() {
    let (_dir, path) = make_test_clip("seek.mp4", 3);
    let mut decoder = Decoder::open(&path).unwrap();

    decoder.seek_to_time(1.5).unwrap();
    let (idx, _) = decoder
        .next_frame()
        .unwrap()
        .expect("there should have been a frame after the seek");

    // The seek lands on the keyframe <= target: with GOP=10 at 25fps the
    // distance from frame 1.5s*25=37 does not exceed one GOP.
    assert!(idx <= 37, "idx={idx} should be <= the target");
    assert!(idx >= 37 - 10, "idx={idx} too far from the target");
}

/// Regression for a real bug, confirmed by the user on a
/// 1080p60fps file with B-frames: `avformat_seek_file`, even constraining
/// `max_ts` to the target (tried and verified ineffective: the mov/mp4
/// demuxer ignores it), can still land on a keyframe *after* the
/// target instead of on the preceding one, when the target falls a
/// frame or two from a keyframe boundary — the concrete case was
/// `target=749` landing on `idx=751` instead of on a much earlier
/// preceding keyframe, leaving the frames in between uncovered
/// forever (see `seek_to_time`, which now corrects itself by retrying
/// further back until it really lands `<=` the target). Not reproduced
/// by the synthetic content below (the real file that triggered the bug
/// had a B-frame structure this `testsrc` does not replicate), but the
/// contract (`idx <= target`) must be respected all the same, near
/// *every* keyframe boundary, not only far from them as in the test
/// above.
#[test]
fn decoder_seek_lands_at_or_before_target_near_every_keyframe_boundary() {
    let (_dir, path) = make_test_clip_with_gop_and_bframes("seek_boundaries.mp4", 4, 25, 3);
    // Keyframes at 0,25,50,75 (fps=25, g=25): a target 1/2/3 frames
    // before each one is the edge case that triggered the bug.
    for keyframe in [25, 50, 75] {
        for offset in [1, 2, 3] {
            let target = keyframe - offset;
            let mut decoder = Decoder::open(&path).unwrap();
            decoder.seek_to_time(target as f64 / 25.0).unwrap();
            let (idx, _) = decoder.next_frame().unwrap().expect("frame expected");
            assert!(
                idx <= target,
                "keyframe={keyframe} offset={offset} target={target}: \
                     landed on idx={idx}, past the target"
            );
        }
    }
}

/// Skipping the transit after a seek (non-reference B-frames undecoded,
/// the rest unconverted) must not change a single pixel of what follows.
#[test]
fn skip_before_hands_out_the_same_frames_as_a_full_decode() {
    let (_dir, path) = make_test_clip_with_gop_and_bframes("skip_before.mp4", 4, 25, 2);
    let mut full = Decoder::open(&path).unwrap();
    let mut reference = std::collections::HashMap::new();
    while let Some((idx, frame)) = full.next_frame().unwrap() {
        reference.insert(idx, frame);
    }

    let mut decoder = Decoder::open(&path).unwrap();
    decoder.seek_to_time(70.0 / 25.0).unwrap();
    assert_eq!(decoder.landing(), Some(50));
    decoder.skip_before(68);
    for expected in 68..=80 {
        let (idx, frame) = decoder.next_frame().unwrap().unwrap();
        assert_eq!(idx, expected);
        assert!(frame.y == reference[&idx].y, "frame {idx} differs");
    }
}

/// A skip in the middle of the stream (no seek before it) must not look like
/// a hole to fill by repeating the last frame handed out.
#[test]
fn skip_before_without_a_seek_resumes_at_the_first_wanted_frame() {
    let (_dir, path) = make_test_clip_with_gop_and_bframes("skip_mid_stream.mp4", 4, 25, 2);
    let mut decoder = Decoder::open(&path).unwrap();
    for _ in 0..=20 {
        decoder.next_frame().unwrap().unwrap();
    }
    decoder.skip_before(40);
    let (idx, _) = decoder.next_frame().unwrap().unwrap();
    assert_eq!(idx, 40);
}

fn decode_all(mut decoder: Decoder) -> Vec<(FrameIdx, Arc<FrameYuv420>)> {
    std::iter::from_fn(|| decoder.next_frame().unwrap()).collect()
}

fn assert_same_frames(got: &[(FrameIdx, Arc<FrameYuv420>)], want: &[(FrameIdx, Arc<FrameYuv420>)]) {
    assert_eq!(got.len(), want.len());
    for ((idx, frame), (want_idx, want_frame)) in got.iter().zip(want) {
        assert_eq!(idx, want_idx);
        assert!(
            frame.y == want_frame.y && planar_chroma(frame) == planar_chroma(want_frame),
            "frame {idx} differs"
        );
    }
}

/// Decoder with a hwaccel that is not there: frames in `pix_fmt` are what
/// the fake "hardware" is expected to produce.
fn fake_hw(path: &Path, pix_fmt: ffmpeg::ffi::AVPixelFormat, fail_after: Option<u32>) -> Decoder {
    let mut decoder = Decoder::open(path).unwrap();
    decoder.hw = Some(crate::hw::HwState {
        pix_fmt,
        device: crate::hw::HwDevice::Cuda,
    });
    decoder.fail_after = fail_after;
    decoder
}

/// The HW budget is global: the tests that open HW decoders or change it
/// take turns.
static HW_BUDGET_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

const ALL_DEVICES: &[crate::hw::HwDevice] = &[
    crate::hw::HwDevice::VideoToolbox,
    crate::hw::HwDevice::Cuda,
    crate::hw::HwDevice::Vulkan(None),
];

/// Without a usable device (CI), or with one, the frames are those of
/// software decoding: the hwaccels decode H.264 bit-exactly and NV12 →
/// YUV420P only moves bytes.
#[test]
fn open_with_hw_decodes_the_same_frames_as_software() {
    let _budget = HW_BUDGET_LOCK.lock().unwrap();
    let (_dir, path) = make_test_clip_with_gop_and_bframes("hw_same_frames.mp4", 2, 25, 2);
    let want = decode_all(Decoder::open(&path).unwrap());
    let hw = Decoder::open_with(&path, ALL_DEVICES, crate::hw::HwPriority::Normal).unwrap();
    assert_same_frames(&decode_all(hw), &want);
}

#[test]
fn a_silent_software_fallback_of_ffmpeg_reopens_the_decoder_in_software() {
    let (_dir, path) = make_test_clip_with_gop_and_bframes("hw_silent_fallback.mp4", 2, 25, 2);
    let want = decode_all(Decoder::open(&path).unwrap());
    let mut decoder = fake_hw(&path, ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_CUDA, None);
    decoder.seek_to_time(0.0).unwrap();
    assert!(!decoder.is_hw());
    assert!(crate::hw::has_failed(&path), "not retried on the next open");
    assert_same_frames(&decode_all(decoder), &want);
}

#[test]
fn a_hw_error_mid_stream_goes_on_in_software_without_changing_the_frames() {
    let (_dir, path) = make_test_clip_with_gop_and_bframes("hw_error_mid_stream.mp4", 2, 25, 2);
    let want = decode_all(Decoder::open(&path).unwrap());
    let decoder = fake_hw(
        &path,
        ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_YUV420P,
        Some(33),
    );
    assert_same_frames(&decode_all(decoder), &want);
}

#[test]
fn a_hw_error_during_a_transit_lands_on_the_wanted_frame() {
    let (_dir, path) = make_test_clip_with_gop_and_bframes("hw_error_transit.mp4", 4, 25, 2);
    let want = decode_all(Decoder::open(&path).unwrap());
    let mut decoder = fake_hw(&path, ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_YUV420P, None);
    decoder.seek_to_time(70.0 / 25.0).unwrap();
    assert_eq!(decoder.landing(), Some(50));
    decoder.skip_before(68);
    decoder.fail_after = Some(5);
    let got: Vec<_> = (0..13)
        .map(|_| decoder.next_frame().unwrap().unwrap())
        .collect();
    assert!(!decoder.is_hw());
    assert_same_frames(&got, &want[68..=80]);
}

/// Process CPU time (user + system), where `/proc` has it.
fn cpu_time() -> Option<std::time::Duration> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // Fields after the `(comm)`, which may contain spaces: utime and stime
    // are the 14th and 15th of the line, in ticks of 1/100 s on Linux.
    let rest = &stat[stat.rfind(')')? + 2..];
    let fields: Vec<&str> = rest.split(' ').collect();
    let ticks: u64 = fields[11].parse::<u64>().ok()? + fields[12].parse::<u64>().ok()?;
    Some(std::time::Duration::from_millis(ticks * 10))
}

/// Software vs every HW backend on `VV_BENCH_CLIP` (a synthetic 1080p HEVC
/// clip without it): open + first frame, then throughput with the transfer
/// to system memory included and reported apart.
/// `VV_BENCH_CLIP=<path> cargo test --release -p vv-media bench_hw_decode -- --ignored --nocapture`.
#[test]
#[ignore = "manual measurement, not a correctness assertion"]
fn bench_hw_decode() {
    use crate::hw::HwDevice;
    let dir = crate::test_support::TempDir::new("vv-media-decode-bench");
    let path = std::env::var("VV_BENCH_CLIP").map_or_else(
        |_| {
            let path = dir.join("hevc_1080p.mkv");
            crate::test_support::ffmpeg(
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=1920x1080:rate=60:duration=12",
                    "-c:v",
                    "libx265",
                    "-preset",
                    "fast",
                    "-g",
                    "250",
                    "-pix_fmt",
                    "yuv420p",
                ],
                &path,
            );
            path
        },
        std::path::PathBuf::from,
    );
    const FRAMES: usize = 1200;
    let backends: [(&str, &[HwDevice]); 4] = [
        ("software", &[]),
        ("NVDEC", &[HwDevice::Cuda]),
        ("Vulkan", &[HwDevice::Vulkan(None)]),
        ("VideoToolbox", &[HwDevice::VideoToolbox]),
    ];
    for (name, devices) in backends {
        if devices.iter().any(|d| !crate::hw::available(d)) {
            eprintln!("{name}: no device");
            continue;
        }
        let start = std::time::Instant::now();
        let mut decoder =
            Decoder::open_with(&path, devices, crate::hw::HwPriority::Normal).unwrap();
        decoder.seek_to_time(0.0).unwrap();
        decoder.next_frame().unwrap().unwrap();
        let first_frame = start.elapsed();
        if !devices.is_empty() && !decoder.is_hw() {
            eprintln!("{name}: fell back to software (codec or size unsupported)");
            continue;
        }

        let cpu_start = cpu_time();
        let start = std::time::Instant::now();
        let transfer_start = decoder.transfer_time();
        let mut count = 0;
        while count < FRAMES && decoder.next_frame().unwrap().is_some() {
            count += 1;
        }
        let elapsed = start.elapsed();
        let cpu = cpu_start.zip(cpu_time()).map(|(a, b)| b - a);
        eprintln!(
            "{name}: open + first frame {first_frame:?}; {count} frames in {elapsed:?} \
             ({:.0} fps), transfer {:?} ({:.2} ms/frame), CPU {cpu:?}",
            count as f64 / elapsed.as_secs_f64(),
            decoder.transfer_time() - transfer_start,
            (decoder.transfer_time() - transfer_start).as_secs_f64() * 1000.0 / count as f64,
        );
    }
}

/// An NV12 source (as hwaccels, some cameras and screen recorders hand it
/// out) skips the scaler and keeps its interleaved chroma, odd sizes
/// included.
#[test]
fn an_nv12_source_keeps_its_interleaved_chroma() {
    let dir = crate::test_support::TempDir::new("vv-media-decode-test");
    let path = dir.join("odd_nv12.mkv");
    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=321x241:rate=25:duration=1",
            "-c:v",
            "rawvideo",
            "-pix_fmt",
            "nv12",
        ],
        &path,
    );
    let reference_path = dir.join("odd_nv12.yuv");
    crate::test_support::ffmpeg(
        &[
            "-i",
            path.to_str().unwrap(),
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "nv12",
        ],
        &reference_path,
    );

    let frame = decode_first_frame(&path).expect("decode failed");
    assert_eq!((frame.chroma_width, frame.chroma_height), (161, 121));
    let Chroma::Interleaved(uv) = &frame.chroma else {
        panic!("NV12 made planar");
    };
    let decoded = [frame.y.as_slice(), uv].concat();
    assert!(decoded == std::fs::read(&reference_path).unwrap());
}

#[test]
fn a_decoder_whose_surfaces_do_not_fit_the_budget_opens_in_software() {
    let _budget = HW_BUDGET_LOCK.lock().unwrap();
    let (_dir, path) = make_test_clip("hw_over_budget.mp4", 1);
    crate::hw::set_budget_bytes(1);
    let decoder = Decoder::open_with(&path, ALL_DEVICES, crate::hw::HwPriority::Normal);
    crate::hw::set_budget_bytes(usize::MAX);
    assert!(!decoder.unwrap().is_hw());
}

#[test]
fn a_low_priority_lease_leaves_half_of_the_budget_to_the_others() {
    use crate::hw::{HwPriority, Lease};
    let _budget = HW_BUDGET_LOCK.lock().unwrap();
    crate::hw::set_budget_bytes(100);
    let low = Lease::take(40, HwPriority::Low).expect("fits in half");
    assert!(Lease::take(20, HwPriority::Low).is_none(), "past half");
    let normal = Lease::take(60, HwPriority::Normal).expect("fits in the whole");
    assert!(Lease::take(1, HwPriority::Normal).is_none(), "budget full");
    drop((low, normal));
    assert!(
        Lease::take(100, HwPriority::Normal).is_some(),
        "given back on drop"
    );
    crate::hw::set_budget_bytes(usize::MAX);
}

#[test]
fn landing_frame_is_the_keyframe_and_still_comes_out_of_next_frame() {
    let (_dir, path) = make_test_clip_with_gop_and_bframes("landing_frame.mp4", 4, 25, 2);
    let mut decoder = Decoder::open(&path).unwrap();
    decoder.seek_to_time(60.0 / 25.0).unwrap();

    let (idx, frame) = decoder.landing_frame().unwrap().expect("a landing");
    assert_eq!(idx, 50, "the keyframe before frame 60");
    assert_eq!(decoder.landing(), Some(50));
    let (next_idx, next) = decoder.next_frame().unwrap().unwrap();
    assert_eq!(next_idx, 50);
    assert_eq!(next.y, frame.y);
}

#[test]
fn landing_frame_is_none_without_a_seek_pending() {
    let (_dir, path) = make_test_clip_with_gop_and_bframes("landing_none.mp4", 1, 25, 0);
    let mut decoder = Decoder::open(&path).unwrap();
    assert!(decoder.landing_frame().unwrap().is_none());
}

#[test]
fn keyframe_before_decodes_the_keyframe_alone_and_a_seek_resumes_after_it() {
    let (_dir, path) = make_test_clip_with_gop_and_bframes("keyframe_before.mp4", 4, 25, 2);
    let mut decoder = Decoder::open(&path).unwrap();
    let mut reference = Decoder::open(&path).unwrap();
    reference.seek_to_time(2.0).unwrap();
    let (_, expected) = reference.next_frame().unwrap().unwrap();

    let (idx, frame) = decoder.keyframe_before(60).unwrap().unwrap();
    assert_eq!(idx, 50);
    assert_eq!(frame.y, expected.y);
    assert_eq!(decoder.keyframe_before(49).unwrap().unwrap().0, 25);
    assert_eq!(decoder.keyframe_before(0).unwrap().unwrap().0, 0);

    decoder.seek_to_time(70.0 / 25.0).unwrap();
    assert_eq!(
        decoder.next_frame().unwrap().unwrap().0,
        50,
        "usable again after a seek"
    );
}
