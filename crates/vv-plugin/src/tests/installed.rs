use std::path::PathBuf;

use super::*;

/// One test: the list is process-global, so the unseeded state is only
/// observable before seeding.
#[test]
fn installed_is_empty_until_seeded_and_never_scans() {
    assert!(installed().is_empty());

    let info = PluginInfo {
        class_id: "AB".into(),
        name: "X".into(),
        vendor: "V".into(),
        version: "1".into(),
        path: PathBuf::from("/x.vst3"),
    };
    set_installed(vec![info.clone()]);
    assert_eq!(&*installed(), &[info]);
}

#[test]
fn the_scan_flag_is_set_and_cleared() {
    assert!(!scanning());
    set_scanning(true);
    assert!(scanning());
    set_scanning(false);
    assert!(!scanning());
}

fn info(class: &str, path: &str, version: &str) -> PluginInfo {
    PluginInfo {
        class_id: class.into(),
        name: "N".into(),
        vendor: "V".into(),
        version: version.into(),
        path: PathBuf::from(path),
    }
}

#[test]
fn the_same_plugins_in_any_order_are_the_same_list() {
    let a = info("A", "/a.vst3", "1");
    let b = info("B", "/b.vst3", "1");
    assert!(same_plugins(
        &[a.clone(), b.clone()],
        &[b.clone(), a.clone()]
    ));
    assert!(same_plugins(&[], &[]));
}

#[test]
fn a_renamed_or_re_vendored_plugin_is_a_changed_list() {
    let a = info("A", "/a.vst3", "1");
    let renamed = PluginInfo {
        name: "Other".into(),
        ..a.clone()
    };
    assert!(!same_plugins(std::slice::from_ref(&a), &[renamed]));
    let re_vendored = PluginInfo {
        vendor: "Other".into(),
        ..a.clone()
    };
    assert!(!same_plugins(&[a], &[re_vendored]));
}

#[test]
fn a_changed_list_is_not_the_same() {
    let a = info("A", "/a.vst3", "1");
    let b = info("B", "/b.vst3", "1");
    assert!(!same_plugins(
        std::slice::from_ref(&a),
        &[a.clone(), b.clone()]
    ));
    assert!(!same_plugins(std::slice::from_ref(&a), &[]));
    assert!(!same_plugins(
        std::slice::from_ref(&a),
        &[info("A", "/a.vst3", "2")]
    ));
    assert!(!same_plugins(
        std::slice::from_ref(&a),
        &[info("A", "/x.vst3", "1")]
    ));
    assert!(!same_plugins(&[a], &[b]));
}
