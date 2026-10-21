use super::{LoadError, Registry, c_str, cached, linux_binary, sdk_arch_dir, so_file_name};
use std::ffi::c_char;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

fn registry() -> Registry<Arc<u32>> {
    Mutex::new(None)
}

#[test]
fn c_str_stops_at_nul_or_array_end() {
    let full = [b'a' as c_char; 4];
    assert_eq!(c_str(&full), "aaaa");
    let short = [b'h' as c_char, b'i' as c_char, 0, b'x' as c_char];
    assert_eq!(c_str(&short), "hi");
}

#[test]
fn a_cached_path_does_not_wait_for_another_path_loading() {
    let reg = registry();
    let a = cached(&reg, Path::new("a"), || Ok(Arc::new(1))).unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let reg = &reg;
    std::thread::scope(|s| {
        s.spawn(move || {
            cached(reg, Path::new("b"), || {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(Arc::new(2))
            })
            .unwrap();
        });
        started_rx.recv().unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        s.spawn(move || {
            let again = cached(reg, Path::new("a"), || panic!("a is cached")).unwrap();
            done_tx.send(again).unwrap();
        });
        let again = done_rx.recv_timeout(Duration::from_secs(5));
        release_tx.send(()).unwrap();
        assert!(Arc::ptr_eq(&again.expect("lookup waited on b's load"), &a));
    });
}

#[test]
fn another_path_loads_while_one_is_loading() {
    let reg = registry();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let reg = &reg;
    std::thread::scope(|s| {
        s.spawn(move || {
            cached(reg, Path::new("slow"), || {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(Arc::new(1))
            })
            .unwrap();
        });
        started_rx.recv().unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        s.spawn(move || {
            done_tx
                .send(cached(reg, Path::new("fast"), || Ok(Arc::new(2))))
                .unwrap();
        });
        let fast = done_rx.recv_timeout(Duration::from_secs(5));
        release_tx.send(()).unwrap();
        assert_eq!(*fast.expect("fast load waited").unwrap(), 2);
    });
}

#[test]
fn concurrent_same_path_loads_once_and_shares_the_arc() {
    let reg = registry();
    let loads = AtomicUsize::new(0);
    let got: Vec<Arc<u32>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                s.spawn(|| {
                    cached(&reg, Path::new("p"), || {
                        loads.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(50));
                        Ok(Arc::new(7))
                    })
                    .unwrap()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(loads.load(Ordering::SeqCst), 1);
    assert!(got.iter().all(|m| Arc::ptr_eq(m, &got[0])));
}

#[test]
fn a_failure_before_dlopen_is_retried() {
    let reg = registry();
    assert!(cached(&reg, Path::new("p"), || Err(LoadError::early("no"))).is_err());
    assert_eq!(
        *cached(&reg, Path::new("p"), || Ok(Arc::new(3))).unwrap(),
        3
    );
}

#[test]
fn a_failure_after_dlopen_is_loaded_once_and_cached() {
    let reg = registry();
    let loads = AtomicUsize::new(0);
    for _ in 0..3 {
        let err = cached(&reg, Path::new("p"), || {
            loads.fetch_add(1, Ordering::SeqCst);
            Err(LoadError::loaded("entry failed"))
        })
        .unwrap_err();
        assert_eq!(err, "entry failed");
    }
    assert_eq!(loads.load(Ordering::SeqCst), 1);
}

#[cfg(target_os = "macos")]
mod macos {
    use crate::instance::{ProcessMode, Vst3Instance};
    use crate::scan::scan;
    use crate::testing::fixture_bundle;
    use std::path::PathBuf;
    use vv_audio::PluginProcessor;
    use vv_core::{PluginState, Vst3Effect};

    const PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>PluginHooksVST3</string>
<key>CFBundleIdentifier</key><string>com.venturi.renamed</string>
<key>CFBundlePackageType</key><string>BNDL</string>
</dict></plist>
"#;

    /// Removes the directory it names when dropped, also when a test panics.
    struct RemoveOnDrop(PathBuf);

    impl Drop for RemoveOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn renamed_bundle() -> (PathBuf, RemoveOnDrop) {
        let src = fixture_bundle().join("Contents/MacOS");
        let binary = std::fs::read_dir(src)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let root = std::env::temp_dir().join(format!("venturi-renamed-{}", std::process::id()));
        let bundle = root.join("Renamed.vst3");
        let dir = bundle.join("Contents/MacOS");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(binary, dir.join("PluginHooksVST3")).unwrap();
        std::fs::write(bundle.join("Contents/Info.plist"), PLIST).unwrap();
        (bundle, RemoveOnDrop(root))
    }

    #[test]
    fn bundle_executable_may_differ_from_the_folder_name() {
        let (bundle, _root) = renamed_bundle();
        let found = scan(&[bundle.parent().unwrap().to_path_buf()]);
        assert_eq!(found.len(), 1, "{found:?}");
        let effect = Vst3Effect {
            component_state: PluginState::new(0.25f32.to_le_bytes().to_vec()),
            ..found[0].effect()
        };
        let instance = Vst3Instance::new(&bundle, &effect, 48_000, ProcessMode::Offline).unwrap();
        let mut audio = vec![1.0f32; 64];
        instance.process(&mut audio, 2);
        assert!(audio.iter().all(|s| (s - 0.5).abs() < 1e-6));
    }

    #[test]
    #[ignore = "needs the installed RX 11 bundles"]
    fn installed_bundles_with_a_custom_executable_are_listed() {
        let found = scan(&crate::scan::default_dirs());
        let rx = found
            .iter()
            .filter(|p| p.path.to_string_lossy().contains("RX 11"))
            .count();
        println!("RX 11 plugins listed: {rx} of {} total", found.len());
        assert!(rx > 0);
    }
}

#[cfg(unix)]
#[test]
fn the_so_name_keeps_the_stem_bytes() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let raw = OsStr::from_bytes(b"Caf\xe9");
    assert_eq!(so_file_name(raw).into_vec(), b"Caf\xe9.so");
    assert_eq!(so_file_name(OsStr::new("Gain")), OsStr::new("Gain.so"));
}

#[test]
fn rust_arch_maps_to_the_sdk_folder_name() {
    for (arch, big, dir) in [
        ("x86_64", false, "x86_64-linux"),
        ("x86", false, "i386-linux"),
        ("aarch64", false, "aarch64-linux"),
        ("arm", false, "armv7l-linux"),
        ("powerpc64", false, "ppc64le-linux"),
        ("powerpc64", true, "powerpc64-linux"),
        ("riscv64", false, "riscv64-linux"),
        ("s390x", true, "s390x-linux"),
    ] {
        assert_eq!(sdk_arch_dir(arch, big), dir, "{arch} big={big}");
    }
}

fn bundle_with(root: &str, files: &[&str]) -> std::path::PathBuf {
    let bundle = std::env::temp_dir().join(format!("{root}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&bundle);
    for f in files {
        let path = bundle.join(f);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"").unwrap();
    }
    bundle
}

#[test]
fn a_missing_arch_folder_falls_back_to_any_linux_folder_with_the_binary() {
    let bundle = bundle_with("vv-linux-fallback", &["Contents/i686-linux/Gain.so"]);
    let stem = std::ffi::OsStr::new("Gain");
    assert_eq!(
        linux_binary(&bundle, stem, "i386-linux"),
        bundle.join("Contents/i686-linux/Gain.so")
    );
    let _ = std::fs::remove_dir_all(&bundle);
}

#[test]
fn the_mapped_folder_wins_and_a_missing_binary_reports_the_mapped_path() {
    let bundle = bundle_with(
        "vv-linux-mapped",
        &[
            "Contents/i386-linux/Gain.so",
            "Contents/i686-linux/Gain.so",
            "Contents/other-linux/Other.so",
        ],
    );
    let gain = std::ffi::OsStr::new("Gain");
    assert_eq!(
        linux_binary(&bundle, gain, "i386-linux"),
        bundle.join("Contents/i386-linux/Gain.so")
    );
    let none = std::ffi::OsStr::new("Missing");
    assert_eq!(
        linux_binary(&bundle, none, "i386-linux"),
        bundle.join("Contents/i386-linux/Missing.so")
    );
    let _ = std::fs::remove_dir_all(&bundle);
}
