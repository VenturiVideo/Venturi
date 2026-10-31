use super::*;

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

/// A still image passes the `duration_frames`/`fps` sentinel (see
/// `vv_core::IMAGE_DURATION_FRAMES`) as `duration_secs` — huge, but
/// `generate_thumbnail` clamps the seek target to `<= 1.0` anyway, so
/// it works without needing `Decoder::open_image`.
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
