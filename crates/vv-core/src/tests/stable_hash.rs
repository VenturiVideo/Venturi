use super::*;

#[test]
fn the_stable_hash_does_not_change() {
    assert_eq!(stable_hash((1u64, 2u32, "levels")), 0x1dc1_4506_b088_1bce);
}
