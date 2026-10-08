use super::*;

#[test]
fn a_new_mask_is_centered_and_half_the_layer() {
    let mask = ClipMask::new(MaskShape::Rectangle, (1920, 1080)).value_at(0);
    assert_eq!(mask.center, [0.0, 0.0]);
    assert_eq!(mask.size, [960.0, 540.0]);
    assert_eq!(mask.opacity, 1.0);
    assert!(mask.polygon.is_empty());
}

#[test]
fn a_path_without_handles_flattens_to_its_corners() {
    let mask = ClipMask::new(MaskShape::Path, (400, 200)).value_at(0);
    assert_eq!(
        mask.polygon,
        [
            [-100.0, 50.0],
            [100.0, 50.0],
            [100.0, -50.0],
            [-100.0, -50.0]
        ]
    );
}

#[test]
fn the_path_follows_center_and_clockwise_rotation() {
    let mut mask = ClipMask::new(MaskShape::Path, (400, 200));
    mask.path = Keyframed::constant(MaskPath {
        points: vec![PathPoint::corner([0.0, 10.0])],
    });
    mask.track_mut(MaskParam::CenterX).default = 5.0;
    mask.track_mut(MaskParam::Rotation).default = 90.0;
    let [x, y] = mask.value_at(0).polygon[0];
    assert!((x - 15.0).abs() < 1e-4, "up turned clockwise points right");
    assert!(y.abs() < 1e-4);
}

#[test]
fn a_curved_segment_adds_points_on_the_bezier() {
    let path = MaskPath {
        points: vec![
            PathPoint {
                out_handle: [0.0, 10.0],
                ..PathPoint::corner([0.0, 0.0])
            },
            PathPoint {
                in_handle: [0.0, 10.0],
                ..PathPoint::corner([10.0, 0.0])
            },
        ],
    };
    let polygon = path.flatten();
    assert_eq!(polygon.len(), 1 + (CURVE_SEGMENTS - 1) + 1);
    let mid = polygon[CURVE_SEGMENTS / 2];
    assert!((mid[0] - 5.0).abs() < 1e-4 && (mid[1] - 7.5).abs() < 1e-4);
}

#[test]
fn paths_interpolate_point_by_point_only_when_they_match() {
    let a = MaskPath {
        points: vec![PathPoint::corner([0.0, 0.0])],
    };
    let b = MaskPath {
        points: vec![PathPoint::corner([10.0, 20.0])],
    };
    assert_eq!(MaskPath::lerp(&a, &b, 0.5).points[0].point, [5.0, 10.0]);
    let c = MaskPath {
        points: vec![PathPoint::corner([1.0, 1.0]); 2],
    };
    assert_eq!(
        MaskPath::lerp(&a, &c, 0.5),
        a,
        "different vertex counts hold"
    );
}
