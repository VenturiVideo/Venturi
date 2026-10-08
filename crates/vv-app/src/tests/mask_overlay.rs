use super::*;

#[test]
fn the_mask_frame_round_trips_layer_points() {
    let frame = Frame {
        center: [10.0, -5.0],
        rotation: 30.0,
    };
    let local = [3.0, 7.0];
    let back = frame.to_local(frame.to_layer(local));
    assert!((back[0] - local[0]).abs() < 1e-4 && (back[1] - local[1]).abs() < 1e-4);
    let up = frame.to_layer([0.0, 1.0]);
    assert!(up[0] > 10.0, "a clockwise turn tips up towards the right");
}

#[test]
fn polygon_contains_handles_concave_shapes() {
    let pos = |x, y| egui::pos2(x, y);
    // The notch at the bottom middle is outside.
    let notched = [
        pos(0.0, 0.0),
        pos(3.0, 0.0),
        pos(3.0, 6.0),
        pos(7.0, 6.0),
        pos(7.0, 0.0),
        pos(10.0, 0.0),
        pos(10.0, 10.0),
        pos(0.0, 10.0),
    ];
    assert!(polygon_contains(pos(5.0, 8.0), &notched));
    assert!(!polygon_contains(pos(5.0, 3.0), &notched));
}

#[test]
fn nearest_edge_finds_the_segment_under_the_pointer() {
    let path = MaskPath {
        points: [[0.0, 0.0], [100.0, 0.0], [100.0, 100.0]]
            .map(PathPoint::corner)
            .to_vec(),
    };
    let identity = |p: [f32; 2]| egui::pos2(p[0], p[1]);
    assert_eq!(
        nearest_edge(egui::pos2(50.0, 2.0), &path, &identity),
        Some(0)
    );
    assert_eq!(
        nearest_edge(egui::pos2(98.0, 50.0), &path, &identity),
        Some(1)
    );
    assert_eq!(
        nearest_edge(egui::pos2(50.0, 51.0), &path, &identity),
        Some(2)
    );
    assert_eq!(nearest_edge(egui::pos2(80.0, 30.0), &path, &identity), None);
}

#[test]
fn auto_handles_follow_the_neighbours() {
    let points = [[0.0, 0.0], [10.0, 10.0], [20.0, 0.0]].map(PathPoint::corner);
    let (in_handle, out_handle) = auto_handles(&points, 1);
    assert_eq!(out_handle, [20.0 / 6.0, 0.0]);
    assert_eq!(in_handle, [-20.0 / 6.0, 0.0]);
}
