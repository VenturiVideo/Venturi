use super::*;

const TIMELINE: (u32, u32) = (1920, 1080);

fn assert_close(a: [f32; 2], b: [f32; 2]) {
    assert!(
        (a[0] - b[0]).abs() < 1e-3 && (a[1] - b[1]).abs() < 1e-3,
        "{a:?} != {b:?}"
    );
}

#[test]
fn a_narrower_source_is_pillarboxed() {
    let b = ClipBox::new(TIMELINE, (1080, 1080), [0.0; 4], [false; 2]);
    assert_close([b.left, b.right], [-540.0, 540.0]);
    assert_close([b.bottom, b.top], [-540.0, 540.0]);
}

#[test]
fn crop_is_scaled_from_source_pixels() {
    let b = ClipBox::new(TIMELINE, (3840, 2160), [200.0, 0.0, 0.0, 100.0], [false; 2]);
    assert_close([b.left, b.bottom], [-860.0, -490.0]);
}

#[test]
fn clip_to_frame_matches_the_shader_geometry() {
    let t = Transform {
        zoom: [2.0, 2.0],
        position: [100.0, 0.0],
        anchor: [-960.0, 540.0],
        ..Default::default()
    };
    // Zooming around the top-left corner leaves it where it is.
    assert_close(clip_to_frame(&t, [-960.0, 540.0]), [-860.0, 540.0]);
    let r = Transform {
        rotation: 90.0,
        ..Default::default()
    };
    assert_close(clip_to_frame(&r, [100.0, 0.0]), [0.0, -100.0]);
}

#[test]
fn moving_the_anchor_changes_only_the_anchor() {
    let start = Transform {
        zoom: [2.0, 1.5],
        rotation: 30.0,
        position: [50.0, -20.0],
        ..Default::default()
    };
    let b = ClipBox::new(TIMELINE, TIMELINE, [0.0; 4], [false; 2]);
    let t = drag_transform(&start, &b, Handle::Anchor, [120.0, 80.0], false);
    assert_close(t.position, start.position);
    assert_close(t.anchor, [120.0, 80.0]);
    assert_close(
        [t.position[0] + t.anchor[0], t.position[1] + t.anchor[1]],
        [170.0, 60.0],
    );
}

#[test]
fn a_corner_follows_the_pointer_and_scales_uniformly() {
    let start = Transform::default();
    let b = ClipBox::new(TIMELINE, TIMELINE, [0.0; 4], [false; 2]);
    let t = drag_transform(&start, &b, Handle::Scale(1.0, 1.0), [960.0, 540.0], false);
    assert_close(t.zoom, [2.0, 2.0]);
    assert_close(clip_to_frame(&t, b.point(1.0, 1.0)), [1920.0, 1080.0]);
}

#[test]
fn clockwise_turn_is_positive_clockwise_and_wraps() {
    assert!((clockwise_turn([0.0, 1.0], [1.0, 0.0]) - 90.0).abs() < 1e-3);
    assert!((clockwise_turn([-1.0, 0.01], [-1.0, -0.01]) + 1.146).abs() < 1e-2);
}

#[test]
fn a_flipped_crop_is_mirrored() {
    let b = ClipBox::new(TIMELINE, TIMELINE, [100.0, 0.0, 0.0, 0.0], [true, false]);
    assert_close([b.left, b.right], [-960.0, 860.0]);
}

#[test]
fn a_side_crops_its_edge_following_the_pointer() {
    let start = Transform::default();
    let b = ClipBox::new(TIMELINE, (3840, 2160), [0.0; 4], [false; 2]);
    let t = drag_transform(&start, &b, Handle::Crop(-1.0, 0.0), [100.0, 300.0], false);
    assert_eq!(t.crop, [200.0, 0.0, 0.0, 0.0]);
    let t = drag_transform(&start, &b, Handle::Crop(0.0, 1.0), [0.0, -50.0], false);
    assert_eq!(t.crop, [0.0, 100.0, 0.0, 0.0]);
    assert_eq!(t.zoom, start.zoom);
}

#[test]
fn crop_follows_zoom_rotation_and_flip() {
    let start = Transform {
        zoom: [2.0, 2.0],
        rotation: 90.0,
        flip: [true, false],
        ..Default::default()
    };
    let b = ClipBox::new(TIMELINE, TIMELINE, start.crop, start.flip);
    // Rotated clockwise, the right side faces down; flipped, it cuts the
    // left of the source.
    let t = drag_transform(&start, &b, Handle::Crop(1.0, 0.0), [0.0, 40.0], false);
    assert_eq!(t.crop, [20.0, 0.0, 0.0, 0.0]);
}

#[test]
fn crop_cannot_go_negative_or_past_the_opposite_side() {
    let start = Transform {
        crop: [0.0, 0.0, 900.0, 0.0],
        ..Default::default()
    };
    let b = ClipBox::new(TIMELINE, TIMELINE, start.crop, start.flip);
    let t = drag_transform(&start, &b, Handle::Crop(-1.0, 0.0), [5000.0, 0.0], false);
    assert_eq!(t.crop[0], 1919.0 - 900.0);
    let t = drag_transform(&start, &b, Handle::Crop(-1.0, 0.0), [-50.0, 0.0], false);
    assert_eq!(t.crop[0], 0.0);
}

#[test]
fn frame_to_clip_inverts_clip_to_frame() {
    let t = Transform {
        zoom: [1.5, 0.75],
        position: [40.0, -12.0],
        rotation: 25.0,
        anchor: [10.0, 5.0],
        ..Transform::default()
    };
    let p = [-30.0, 70.0];
    let back = frame_to_clip(&t, clip_to_frame(&t, p));
    assert!((back[0] - p[0]).abs() < 1e-3 && (back[1] - p[1]).abs() < 1e-3);
}
