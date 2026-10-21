use super::{FIXTURE_CLASS_ID, build_bundle, fixture_bundle, fixture_dir, target_dir};
use crate::scan;

#[test]
fn fixture_scans_as_effect() {
    let bundle = fixture_bundle();
    let found = scan(&[fixture_dir()]);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].name, "Venturi Test Gain");
    assert_eq!(found[0].vendor, "Venturi");
    assert_eq!(found[0].class_id, FIXTURE_CLASS_ID);
    assert_eq!(found[0].path, bundle);
}

#[test]
fn fixture_bundle_is_inside_the_target_dir() {
    let bundle = fixture_bundle();
    assert!(bundle.starts_with(target_dir()), "{bundle:?}");
}

#[test]
fn concurrent_builds_return_a_loadable_bundle() {
    let handles: Vec<_> = (0..4).map(|_| std::thread::spawn(build_bundle)).collect();
    let bundles: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert!(bundles.windows(2).all(|w| w[0] == w[1]), "{bundles:?}");
    let found = scan(&[fixture_dir()]);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].path, bundles[0]);
}
