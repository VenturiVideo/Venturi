use super::*;

const TIMELINE: (u32, u32) = (1920, 1080);

fn assert_close(got: Option<[f32; 2]>, expected: [f32; 2]) {
    let got = got.expect("inside the clip");
    assert!(
        (got[0] - expected[0]).abs() < 1e-2 && (got[1] - expected[1]).abs() < 1e-2,
        "{got:?} != {expected:?}"
    );
}

#[test]
fn the_frame_maps_onto_a_source_of_the_same_aspect() {
    let t = Transform::default();
    assert_close(
        source_point(&t, TIMELINE, (3840, 2160), [0.0, 0.0]),
        [1920.0, 1080.0],
    );
    assert_close(
        source_point(&t, TIMELINE, (3840, 2160), [-960.0, 540.0]),
        [0.0, 0.0],
    );
}

#[test]
fn the_bars_of_a_narrower_source_are_not_the_clip() {
    let t = Transform::default();
    assert_eq!(
        source_point(&t, TIMELINE, (1080, 1080), [-700.0, 0.0]),
        None
    );
    assert_close(
        source_point(&t, TIMELINE, (1080, 1080), [-540.0, 0.0]),
        [0.0, 540.0],
    );
}

#[test]
fn zoom_and_flip_are_undone() {
    let zoomed = Transform {
        zoom: [2.0, 2.0],
        ..Transform::default()
    };
    assert_close(
        source_point(&zoomed, TIMELINE, TIMELINE, [480.0, 0.0]),
        [1200.0, 540.0],
    );
    let flipped = Transform {
        flip: [true, false],
        ..Transform::default()
    };
    assert_close(
        source_point(&flipped, TIMELINE, TIMELINE, [-480.0, 0.0]),
        [1440.0, 540.0],
    );
}

#[test]
fn the_cropped_away_part_is_not_the_clip() {
    let t = Transform {
        crop: [100.0, 0.0, 0.0, 0.0],
        ..Transform::default()
    };
    assert_eq!(source_point(&t, TIMELINE, TIMELINE, [-900.0, 0.0]), None);
    assert!(source_point(&t, TIMELINE, TIMELINE, [-800.0, 0.0]).is_some());
}

/// Full-range BT.709 frame: green on the left half, red on the right.
fn green_and_red(width: u32, height: u32) -> FrameYuv420 {
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    let row =
        |left: u8, right: u8, w: u32| (0..w).map(move |x| if x < w / 2 { left } else { right });
    let plane = |left, right, w, h| (0..h).flat_map(|_| row(left, right, w)).collect::<Vec<_>>();
    FrameYuv420 {
        width,
        height,
        y: plane(182, 54, width, height),
        chroma: vv_media::Chroma::Planar {
            u: plane(30, 99, cw, ch),
            v: plane(12, 255, cw, ch),
        },
        chroma_width: cw,
        chroma_height: ch,
        matrix: vv_media::ColorMatrix::Bt709,
        full_range: true,
        alpha: None,
    }
}

#[test]
fn a_proxy_is_sampled_where_the_source_point_falls() {
    let proxy = green_and_red(8, 4);
    let source = (1920, 1080);
    let left = sample(&proxy, source, [200.0, 540.0]);
    let right = sample(&proxy, source, [1700.0, 540.0]);
    assert!(left.g > 0.9 && left.r < 0.1, "{left:?}");
    assert!(right.r > 0.9 && right.g < 0.1, "{right:?}");
}
