use super::*;
use std::path::{Path, PathBuf};

const FIXTURE: &str = include_str!("moduleinfo.json");

#[test]
fn parses_effect_classes_from_moduleinfo() {
    let path = Path::new("/plugins/Acme.vst3");
    let infos = parse_module_info(FIXTURE, path);
    assert_eq!(
        infos,
        Some(vec![PluginInfo {
            class_id: "56565445535447414E00000000000001".into(),
            name: "Acme EQ".into(),
            vendor: "Acme Audio".into(),
            version: "1.2.3".into(),
            path: path.to_path_buf(),
        }])
    );
}

#[test]
fn tolerates_trailing_commas() {
    let json = r#"{"Classes": [{"CID": "AA", "Category": "Audio Module Class",
        "Name": "a,]", "Sub Categories": ["Fx",],},],}"#;
    let infos = parse_module_info(json, Path::new("x.vst3"));
    let infos = infos.unwrap();
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].name, "a,]");
}

#[test]
fn scan_skips_missing_dirs() {
    assert!(scan(&[PathBuf::from("/nonexistent/venturi/vst3")]).is_empty());
}

#[test]
fn instrument_only_moduleinfo_is_trusted_without_loading() {
    let json = r#"{"Classes": [{"CID": "AA", "Category": "Audio Module Class",
        "Name": "s", "Sub Categories": ["Instrument"]}]}"#;
    assert_eq!(parse_module_info(json, Path::new("x.vst3")), Some(vec![]));
    assert_eq!(parse_module_info("not json", Path::new("x.vst3")), None);
}

#[test]
fn scan_descends_into_vendor_folders_but_not_bundles() {
    let root = std::env::temp_dir().join(format!("vv-plugin-scan-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let nested = root.join("Vendor/Acme.vst3/Contents");
    std::fs::create_dir_all(nested.join("Resources")).unwrap();
    std::fs::write(nested.join("Resources/moduleinfo.json"), FIXTURE).unwrap();
    let hidden = root.join("Vendor/Acme.vst3/Contents/Inner.vst3/Contents/Resources");
    std::fs::create_dir_all(&hidden).unwrap();
    std::fs::write(hidden.join("moduleinfo.json"), FIXTURE).unwrap();
    let infos = scan(std::slice::from_ref(&root));
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].path, root.join("Vendor/Acme.vst3"));
}

#[test]
#[ignore = "needs plugins installed on this machine"]
fn scans_installed_plugins() {
    let infos = scan(&default_dirs());
    if cfg!(target_os = "macos") {
        assert!(
            infos.iter().any(|i| i.name.starts_with("TDR Nova")),
            "{infos:?}"
        );
    }
}

fn guard_dirs(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("vv-guard-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let plugins = root.join("plugins");
    std::fs::create_dir_all(&plugins).unwrap();
    (root.clone(), plugins, root.join("scan.marker"))
}

#[test]
fn a_clean_guarded_scan_finds_the_plugin_and_leaves_no_marker() {
    let (root, plugins, marker) = guard_dirs("clean");
    let bundle = crate::testing::fixture_bundle();
    let copy = plugins.join(bundle.file_name().unwrap());
    copy_dir(&bundle, &copy);
    let infos = scan_with_guard(&[plugins], &marker, &[]);
    let marker_left = marker.exists();
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(infos.len(), 1, "{infos:?}");
    assert!(!marker_left);
}

#[test]
fn a_blocked_bundle_is_not_loaded() {
    let (root, plugins, marker) = guard_dirs("blocked");
    let bad = plugins.join("Bad.vst3");
    copy_dir(&crate::testing::fixture_bundle(), &bad);
    let infos = scan_with_guard(&[plugins], &marker, std::slice::from_ref(&bad));
    let _ = std::fs::remove_dir_all(&root);
    assert!(infos.is_empty());
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let dest = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), dest).unwrap();
        }
    }
}

#[cfg(unix)]
#[test]
fn a_non_utf8_path_survives_the_marker_exactly() {
    use std::os::unix::ffi::OsStringExt;
    let (root, _plugins, marker) = guard_dirs("bytes");
    let weird = PathBuf::from(std::ffi::OsString::from_vec(
        b" /p/\xff caf\xe9 .vst3 ".to_vec(),
    ));
    std::fs::write(&marker, super::path_to_bytes(&weird)).unwrap();
    let crashed = take_crashed(&marker);
    let marker_left = marker.exists();
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(crashed, Some(weird));
    assert!(!marker_left);
}

#[test]
fn a_path_with_surrounding_spaces_is_not_trimmed() {
    let (root, _plugins, marker) = guard_dirs("spaces");
    let odd = PathBuf::from(" /p/a b .vst3 ");
    std::fs::write(&marker, super::path_to_bytes(&odd)).unwrap();
    let crashed = take_crashed(&marker);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(crashed, Some(odd));
}

#[test]
fn abandoning_the_scan_removes_the_marker_of_a_bundle_mid_load() {
    let (root, _plugins, marker) = guard_dirs("abandon");
    std::fs::write(&marker, b"/p/Slow.vst3").unwrap();
    abandon_scan(&marker);
    let marker_left = marker.exists();
    let crashed = take_crashed(&marker);
    let _ = std::fs::remove_dir_all(&root);
    assert!(!marker_left);
    assert_eq!(crashed, None);
}

#[test]
fn a_scan_abandoned_at_quit_neither_loads_bundles_nor_rewrites_the_marker() {
    let (root, plugins, marker) = guard_dirs("abandoned-scan");
    let bundle = crate::testing::fixture_bundle();
    copy_dir(&bundle, &plugins.join(bundle.file_name().unwrap()));
    abandon_scan(&marker);
    let infos = scan_with_guard(&[plugins], &marker, &[]);
    let marker_left = marker.exists();
    let _ = std::fs::remove_dir_all(&root);
    assert!(infos.is_empty(), "{infos:?}");
    assert!(!marker_left);
}

#[test]
fn abandoning_one_marker_leaves_other_scans_running() {
    let (root, plugins, marker) = guard_dirs("abandon-other");
    abandon_scan(&root.join("other.marker"));
    let bundle = crate::testing::fixture_bundle();
    copy_dir(&bundle, &plugins.join(bundle.file_name().unwrap()));
    let infos = scan_with_guard(&[plugins], &marker, &[]);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(infos.len(), 1, "{infos:?}");
}

fn loading_since(marker: &Path, since: Instant) {
    std::fs::write(marker, b"/p/Slow.vst3").unwrap();
    lock_state().loading.push((marker.to_path_buf(), since));
}

#[test]
fn a_load_under_the_hang_threshold_is_cleared_at_quit_and_a_hung_one_stays_blamed() {
    let (root, _plugins, marker) = guard_dirs("hang");
    let hang = Duration::from_secs(30);
    let started = Instant::now();
    loading_since(&marker, started);
    abandon_scan_at(&marker, started + Duration::from_secs(29), hang);
    let short_left = marker.exists();

    let hung_marker = root.join("hung.marker");
    loading_since(&hung_marker, started);
    abandon_scan_at(&hung_marker, started + hang, hang);
    let hung_left = hung_marker.exists();
    let crashed = take_crashed(&hung_marker);
    let _ = std::fs::remove_dir_all(&root);
    assert!(!short_left);
    assert!(hung_left);
    assert_eq!(crashed, Some(PathBuf::from("/p/Slow.vst3")));
}

fn info_json(cid: &str) -> String {
    format!(
        r#"{{"Classes": [{{"CID": "{cid}", "Category": "Audio Module Class",
        "Name": "n", "Sub Categories": ["Fx"]}}]}}"#
    )
}

fn json_bundle(dir: &Path, name: &str, cid: &str) {
    let res = dir.join(name).join("Contents/Resources");
    std::fs::create_dir_all(&res).unwrap();
    std::fs::write(res.join("moduleinfo.json"), info_json(cid)).unwrap();
}

/// Removes its tree when dropped, also when the test panics.
struct TempTree(PathBuf);

impl std::ops::Deref for TempTree {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fresh_dir(tag: &str) -> TempTree {
    let root = std::env::temp_dir().join(format!("vv-scan-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    TempTree(root)
}

#[cfg(unix)]
#[test]
fn a_root_symlinked_to_another_root_lists_each_plugin_once() {
    let root = fresh_dir("alias");
    let lib = root.join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    json_bundle(&lib, "A.vst3", "AA");
    json_bundle(&lib.join("Vendor"), "B.vst3", "BB");
    std::os::unix::fs::symlink(&lib, root.join("lib64")).unwrap();
    let dirs = [lib.clone(), root.join("lib64")];
    assert_eq!(bundles_in(&dirs).len(), 2);
    let infos = scan(&dirs);
    assert_eq!(infos.len(), 2);
    assert!(infos.iter().all(|i| i.path.starts_with(&lib)));
}

#[cfg(unix)]
#[test]
fn a_symlink_loop_terminates_and_lists_the_bundle_once() {
    let root = fresh_dir("loop");
    json_bundle(&root, "A.vst3", "AA");
    std::os::unix::fs::symlink(&*root, root.join("a")).unwrap();
    std::os::unix::fs::symlink(&*root, root.join("b")).unwrap();
    assert_eq!(scan(&[root.to_path_buf()]).len(), 1);
}

#[cfg(unix)]
#[test]
fn a_bundle_reached_through_two_links_is_listed_once() {
    let root = fresh_dir("twolinks");
    json_bundle(&root.join("real"), "A.vst3", "AA");
    std::os::unix::fs::symlink(root.join("real"), root.join("l1")).unwrap();
    std::os::unix::fs::symlink(root.join("real/A.vst3"), root.join("A2.vst3")).unwrap();
    assert_eq!(bundles_in(&[root.to_path_buf()]).len(), 1);
    assert_eq!(scan(&[root.to_path_buf()]).len(), 1);
}

#[test]
fn the_same_class_id_in_two_bundles_is_listed_once() {
    let root = fresh_dir("sameclass");
    json_bundle(&root, "A.vst3", "AA");
    json_bundle(&root.join("V"), "A.vst3", "AA");
    json_bundle(&root, "B.vst3", "BB");
    let infos = scan(&[root.to_path_buf()]);
    assert_eq!(infos.len(), 2);
    assert_eq!(infos[0].path, root.join("A.vst3"));
}

#[test]
fn a_lowercase_moduleinfo_cid_is_listed_in_the_factory_form() {
    let json = r#"{"Classes": [{"CID": "56565445535447414e00000000000001", "Category": "Audio Module Class",
        "Name": "Acme EQ"}]}"#;
    let infos = parse_module_info(json, Path::new("x.vst3")).unwrap();
    assert_eq!(infos[0].class_id, "56565445535447414E00000000000001");
}

#[test]
fn a_cached_plugin_info_with_a_lowercase_id_loads_canonical() {
    let json = r#"{"class_id": "56565445535447414e00000000000001", "name": "Acme EQ",
        "vendor": "Acme", "version": "1.0", "path": "/plugins/Acme.vst3"}"#;
    let info: PluginInfo = serde_json::from_str(json).unwrap();
    assert_eq!(info.class_id, "56565445535447414E00000000000001");
}

#[test]
fn a_scanned_plugin_becomes_a_fresh_empty_effect() {
    let info = PluginInfo {
        class_id: "ab12".into(),
        name: "Comp".into(),
        vendor: "Acme".into(),
        version: "3".into(),
        path: PathBuf::from("/c.vst3"),
    };
    let (a, b) = (info.effect(), info.effect());
    assert_ne!(a.id, b.id);
    assert_eq!(a.class_id, "AB12");
    assert_eq!(
        (a.name.as_str(), a.vendor.as_str(), a.version.as_str()),
        ("Comp", "Acme", "3")
    );
    assert_eq!(a.component_state, vv_core::PluginState::default());
    assert_eq!(a.controller_state, vv_core::PluginState::default());
}
