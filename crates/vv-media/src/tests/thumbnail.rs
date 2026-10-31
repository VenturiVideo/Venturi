use super::*;

#[test]
fn reported_jpeg_generates_a_thumbnail() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/tests/fixtures/issue-61-original.jpg");
    let thumb = generate_thumbnail(&path, 40_000_000.0, 96).unwrap();
    assert_eq!((thumb.width, thumb.height), (96, 72));
    assert_eq!(thumb.rgba.len(), 96 * 72 * 4);
    assert!(
        thumb
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel[3] == 255)
    );
    assert!(
        thumb
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel != &thumb.rgba[..4])
    );
}

#[test]
fn generate_thumbnail_scales_down_preserving_aspect_ratio() {
    let dir = crate::test_support::TempDir::new("vv-media-thumbnail-test");
    let path = dir.join("red.mp4");
    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=red:size=640x360:rate=25:duration=2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ],
        &path,
    );

    let thumb = generate_thumbnail(&path, 2.0, 96).unwrap();
    assert_eq!((thumb.width, thumb.height), (96, 54));
    assert_eq!(thumb.rgba.len(), 96 * 54 * 4);
    let center = &thumb.rgba[(27 * 96 + 48) * 4..][..3];
    assert!(
        center[0] > 200 && center[1] < 60 && center[2] < 60,
        "{center:?}"
    );
}

/// Still-image thumbnails must work with the sentinel duration used by the pool.
#[test]
fn generate_thumbnail_works_on_a_still_image_despite_the_sentinel_duration() {
    let dir = crate::test_support::TempDir::new("vv-media-thumbnail-test");
    let path = dir.join("red.png");
    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=red:size=640x360:rate=1:duration=1",
            "-frames:v",
            "1",
            "-update",
            "1",
        ],
        &path,
    );

    let sentinel_secs = vv_core::IMAGE_DURATION_FRAMES as f64 / crate::probe::IMAGE_FPS.as_f64();
    let thumb = generate_thumbnail(&path, sentinel_secs, 96).unwrap();
    assert_eq!((thumb.width, thumb.height), (96, 54));
    let center = &thumb.rgba[(27 * 96 + 48) * 4..][..3];
    assert!(
        center[0] > 200 && center[1] < 60 && center[2] < 60,
        "{center:?}"
    );
}

#[test]
fn generate_thumbnail_reads_a_jpeg_without_seeking_past_its_only_frame() {
    struct Fixture(std::path::PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let dir = Fixture(
        std::env::temp_dir().join(format!("vv-jpeg-thumbnail-test-{}", std::process::id(),)),
    );
    std::fs::create_dir_all(&dir.0).unwrap();
    let path = dir.0.join("01.jpg");
    crate::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=red:size=640x360:rate=1:duration=1",
            "-frames:v",
            "1",
            "-c:v",
            "mjpeg",
            "-pix_fmt",
            "yuvj420p",
            "-update",
            "1",
        ],
        &path,
    );
    let sentinel = vv_core::IMAGE_DURATION_FRAMES as f64 / crate::probe::IMAGE_FPS.as_f64();
    let thumb = generate_thumbnail(&path, sentinel, 96).unwrap();
    assert_eq!((thumb.width, thumb.height), (96, 54));
    assert_eq!(thumb.rgba.len(), 96 * 54 * 4);
    let center = &thumb.rgba[(27 * 96 + 48) * 4..][..3];
    assert!(
        center[0] > 200 && center[1] < 60 && center[2] < 60,
        "{center:?}"
    );
}
