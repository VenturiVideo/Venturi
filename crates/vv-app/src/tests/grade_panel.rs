use super::*;

#[test]
fn the_wheels_go_one_two_or_four_per_row() {
    let width = |columns: f32| columns * WHEEL_BLOCK_WIDTH + (columns - 1.0) * 8.0;
    assert_eq!(wheel_columns(width(1.0) - 10.0, 8.0), 1);
    assert_eq!(wheel_columns(width(1.0), 8.0), 1);
    assert_eq!(wheel_columns(width(2.0), 8.0), 2);
    assert_eq!(wheel_columns(width(3.0), 8.0), 2, "never three and one");
    assert_eq!(wheel_columns(width(4.0), 8.0), 4);
}

#[test]
fn a_wheel_keyframe_row_follows_any_of_its_params() {
    let key = |on_keyframe, prev, next| RowKeyframe {
        on_keyframe,
        prev,
        next,
    };
    let joint = joint_keyframe([
        key(false, Some(3), None),
        key(true, Some(7), Some(20)),
        key(false, None, Some(12)),
    ]);
    assert!(joint.on_keyframe);
    assert_eq!(joint.prev, Some(7));
    assert_eq!(joint.next, Some(12));
}
