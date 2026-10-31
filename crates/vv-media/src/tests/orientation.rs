use super::Orientation;
use crate::*;
use std::path::PathBuf;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "vv-orientation-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("cannot create orientation fixture: {e}"),
            }
        }
    }

    fn clip(&self, angle: i32, mirror: bool) -> PathBuf {
        let base = self.0.join("source.mp4");
        if !base.exists() {
            test_support::ffmpeg(
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=64x48:rate=5:duration=1",
                    "-c:v",
                    "libx264",
                    "-qp",
                    "0",
                    "-g",
                    "1",
                    "-pix_fmt",
                    "yuv420p",
                ],
                &base,
            );
        }
        let path = self.0.join(format!("oriented-{angle}-{mirror}.mov"));
        let angle = angle.to_string();
        let mut args = vec!["-display_rotation", &angle];
        if mirror {
            args.push("-display_hflip");
        }
        args.extend(["-i", base.to_str().unwrap(), "-c", "copy"]);
        test_support::ffmpeg(&args, &path);
        let output = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=width,height:stream_side_data=rotation",
                "-of",
                "default=noprint_wrappers=1",
            ])
            .arg(&path)
            .output()
            .unwrap();
        assert!(output.status.success());
        let metadata = String::from_utf8(output.stdout).unwrap();
        assert!(metadata.lines().any(|line| line == "width=64"));
        assert!(metadata.lines().any(|line| line == "height=48"));
        if angle != "0" || mirror {
            assert!(
                metadata.lines().any(|line| line.starts_with("rotation=")),
                "fixture needs a real container display matrix"
            );
        }
        path
    }

    fn reference(&self, path: &std::path::Path, seconds: &str) -> Vec<u8> {
        let output = self.0.join("reference.yuv");
        test_support::ffmpeg(
            &[
                "-i",
                path.to_str().unwrap(),
                "-ss",
                seconds,
                "-frames:v",
                "1",
                "-pix_fmt",
                "yuv420p",
                "-f",
                "rawvideo",
            ],
            &output,
        );
        std::fs::read(output).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn planar(frame: &FrameYuv420) -> Vec<u8> {
    let mut bytes = frame.y.clone();
    for channel in 0..2 {
        for y in 0..frame.chroma_height as usize {
            for x in 0..frame.chroma_width as usize {
                let (u, v) = frame.chroma_at(x, y);
                bytes.push(if channel == 0 { u } else { v });
            }
        }
    }
    bytes
}

#[test]
fn rotated_video_metadata_uses_display_dimensions() {
    let fixture = Fixture::new();
    for angle in [90, -90, 180, 0] {
        let path = fixture.clip(angle, false);
        let expected = if angle.abs() == 90 {
            (48, 64)
        } else {
            (64, 48)
        };
        let meta = probe(&path).unwrap();
        assert_eq!((meta.width, meta.height), expected, "rotation {angle}");
        let decoder = Decoder::open(&path).unwrap();
        assert_eq!((decoder.width(), decoder.height()), expected);
    }
}

#[test]
fn oriented_decoding_matches_ffmpeg_before_and_after_seeks() {
    let fixture = Fixture::new();
    for angle in [90, -90, 180, 0] {
        for mirror in [false, true] {
            let path = fixture.clip(angle, mirror);
            let expected = if angle.abs() == 90 {
                (48, 64)
            } else {
                (64, 48)
            };
            let mut decoder = Decoder::open(&path).unwrap();
            let (_, first) = decoder.next_frame().unwrap().unwrap();
            assert_eq!((first.width, first.height), expected);
            assert_eq!(
                planar(&first),
                fixture.reference(&path, "0"),
                "rotation {angle}, mirror {mirror}"
            );
            decoder.seek_to_time(0.4).unwrap();
            let (_, seeked) = decoder.next_frame().unwrap().unwrap();
            assert_eq!(
                planar(&seeked),
                fixture.reference(&path, "0.4"),
                "seek: rotation {angle}, mirror {mirror}"
            );
        }
    }
}

#[test]
fn thumbnail_and_proxy_keep_the_display_orientation_once() {
    let fixture = Fixture::new();
    let path = fixture.clip(90, false);
    let thumbnail = generate_thumbnail(&path, 1.0, 100).unwrap();
    assert_eq!((thumbnail.width, thumbnail.height), (48, 64));
    let hash = 0x8200_0000 ^ u64::from(std::process::id());
    let quality = proxy::ProxyQuality::High;
    let proxy = proxy::generate_proxy(&path, hash, quality, &[], |_| true).unwrap();
    let meta = probe(&proxy).unwrap();
    assert_eq!((meta.width, meta.height), (48, 64));
    let mut decoder = Decoder::open(&proxy).unwrap();
    let (_, frame) = decoder.next_frame().unwrap().unwrap();
    let reference = fixture.reference(&path, "0");
    let error: f64 = frame
        .y
        .iter()
        .zip(&reference)
        .map(|(&a, &b)| f64::from(a.abs_diff(b)))
        .sum::<f64>()
        / frame.y.len() as f64;
    assert!(error < 6.0, "proxy orientation or luma changed: {error}");
    drop(decoder);
    std::fs::remove_file(proxy).unwrap();
}

#[test]
fn rotated_previews_respect_display_space_size_limits() {
    let fixture = Fixture::new();
    let path = fixture.clip(90, false);
    let mut decoder = Decoder::open(&path).unwrap();
    decoder.set_max_size(Some((32, 64)));
    let (_, frame) = decoder.next_frame().unwrap().unwrap();
    assert_eq!((frame.width, frame.height), (32, 42));
}

#[test]
fn malformed_and_nonorthogonal_matrices_are_not_quarter_turns() {
    assert!(Orientation::from_bytes(&[0; 35]).is_none());
    assert!(Orientation::from_bytes(&[0; 36]).is_none());
    let matrix: [i32; 9] = [46341, 46341, 0, -46341, 46341, 0, 0, 0, 1 << 30];
    let bytes: Vec<_> = matrix.into_iter().flat_map(i32::to_ne_bytes).collect();
    assert!(Orientation::from_bytes(&bytes).is_none());
}

#[test]
fn rotation_preserves_odd_nv12_chroma_and_alpha() {
    let frame = FrameYuv420 {
        width: 3,
        height: 2,
        y: vec![1, 2, 3, 4, 5, 6],
        chroma: Chroma::Interleaved(vec![10, 11, 20, 21]),
        chroma_width: 2,
        chroma_height: 1,
        matrix: ColorMatrix::Bt709,
        full_range: true,
        alpha: Some(vec![1, 2, 3, 4, 5, 6]),
    };
    let rotated = Orientation([0, 1, -1, 0]).apply(frame);
    assert_eq!((rotated.width, rotated.height), (2, 3));
    assert_eq!(rotated.y, vec![4, 1, 5, 2, 6, 3]);
    assert_eq!(rotated.alpha, Some(vec![4, 1, 5, 2, 6, 3]));
    assert_eq!((rotated.chroma_width, rotated.chroma_height), (1, 2));
    assert_eq!(rotated.chroma_at(0, 0), (10, 11));
    assert_eq!(rotated.chroma_at(0, 1), (20, 21));
    assert_eq!(rotated.matrix, ColorMatrix::Bt709);
    assert!(rotated.full_range);
}
