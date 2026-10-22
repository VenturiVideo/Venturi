use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use vv_core::{EffectId, PluginState, Vst3Effect};

use crate::{HostedPlugin, Instances, PluginInfo, ProcessMode, Vst3Instance};

const BUNDLE: &str = "VenturiTestGain";

/// The gain class of the fixture plugin.
pub const FIXTURE_CLASS_ID: &str = "56565445535447414E00000000000001";

/// The fixture gain plugin as a `.vst3` bundle, built once per process.
pub fn fixture_bundle() -> PathBuf {
    static BUNDLE_PATH: OnceLock<PathBuf> = OnceLock::new();
    BUNDLE_PATH.get_or_init(build_bundle).clone()
}

/// The directory to scan to find only the fixture bundle.
pub fn fixture_dir() -> PathBuf {
    fixture_bundle()
        .parent()
        .expect("bundle has a parent")
        .to_path_buf()
}

/// What a scan reports for the fixture, pointing at a stand-in path: no bundle is built.
pub fn fixture_info() -> PluginInfo {
    PluginInfo {
        class_id: FIXTURE_CLASS_ID.into(),
        name: "Venturi Test Gain".into(),
        vendor: "Venturi".into(),
        version: "1.0.0".into(),
        path: PathBuf::from("/fake.vst3"),
    }
}

/// The fixture gain with `state` as its component state (the gain is twice its first f32).
pub fn fixture_effect(state: &[u8]) -> Vst3Effect {
    Vst3Effect {
        id: EffectId(1),
        component_state: PluginState::new(state.to_vec()),
        ..fixture_info().effect()
    }
}

/// `fixture_effect` under another class id, as written (not canonicalised).
pub fn fixture_effect_of(class_id: &str, state: &[u8]) -> Vst3Effect {
    Vst3Effect {
        class_id: class_id.into(),
        ..fixture_effect(state)
    }
}

pub fn fixture_instance(effect: &Vst3Effect) -> Arc<Vst3Instance> {
    Vst3Instance::new(&fixture_bundle(), effect, 48_000, ProcessMode::Realtime)
        .expect("fixture instance")
}

pub struct Passthrough;

impl vv_audio::PluginProcessor for Passthrough {
    fn process(&self, _: &mut [f32], _: usize) -> bool {
        true
    }
    fn reset(&self) {}
    fn latency(&self) -> u32 {
        0
    }
}

impl HostedPlugin for Passthrough {
    fn set_state(&self, _: &Vst3Effect) {}
    fn state_hash(&self) -> u64 {
        0
    }
    fn mark_state(&self, _: u64) {}
}

/// A pool that knows the fixture class and counts the calls to `make`.
pub fn counting_pool(
    mut make: impl FnMut() -> Option<Arc<dyn HostedPlugin>> + Send + 'static,
) -> (Instances, Arc<AtomicUsize>) {
    let made = Arc::new(AtomicUsize::new(0));
    let counter = made.clone();
    let pool = Instances::with_maker(
        vec![fixture_info()].into(),
        Box::new(move |_, _, _| {
            counter.fetch_add(1, Ordering::SeqCst);
            make()
        }),
    );
    (pool, made)
}

/// Where the fixture is built: the cargo target dir.
fn target_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"))
}

fn build_bundle() -> PathBuf {
    let target = target_dir();
    // Own target dir: the outer cargo holds the lock on the shared one while tests run.
    let output = Command::new(env!("CARGO"))
        .args(["build", "-p", "vv-plugin-fixture", "--message-format=json"])
        .arg("--target-dir")
        .arg(target.join("fixture"))
        .output()
        .expect("failed to run cargo");
    assert!(
        output.status.success(),
        "building vv-plugin-fixture failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let cdylib = cdylib_path(&String::from_utf8_lossy(&output.stdout)).unwrap_or_else(|| {
        panic!(
            "no cdylib in cargo output:\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    });

    // Cargo re-uplifts target/<profile>/ on every build, briefly removing the file a concurrent copy reads.
    let cdylib = deps_artifact(&cdylib);

    // One bundle per target dir (removed by `cargo clean`). Every process writes the same
    // bytes to its own temp file and renames it over the binary, so readers never see a
    // partial file and a dylib already loaded keeps its old inode.
    let root = target.join("fixture").join("bundle");
    let bundle = root.join(format!("{BUNDLE}.vst3"));
    let (dir, file) = if cfg!(target_os = "macos") {
        ("MacOS".to_string(), BUNDLE.to_string())
    } else {
        (
            format!("{}-linux", std::env::consts::ARCH),
            format!("{BUNDLE}.so"),
        )
    };
    let bin_dir = bundle.join("Contents").join(dir);
    std::fs::create_dir_all(&bin_dir).expect("create bundle dir");
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    let call = CALLS.fetch_add(1, Ordering::Relaxed);
    let tmp = root.join(format!(".{BUNDLE}-{}-{call}.tmp", std::process::id()));
    std::fs::copy(&cdylib, &tmp).expect("copy fixture binary");
    std::fs::rename(&tmp, bin_dir.join(file)).expect("install fixture binary");
    bundle
}

/// The copy of `uplifted` that cargo keeps in `deps/`, or `uplifted` if that is missing.
fn deps_artifact(uplifted: &Path) -> PathBuf {
    let deps = uplifted
        .parent()
        .expect("artifact has a parent")
        .join("deps")
        .join(uplifted.file_name().expect("artifact has a name"));
    if deps.exists() {
        deps
    } else {
        uplifted.to_path_buf()
    }
}

fn cdylib_path(cargo_json: &str) -> Option<PathBuf> {
    cargo_json
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|m| {
            m["reason"] == "compiler-artifact" && m["target"]["name"] == "vv_plugin_fixture"
        })
        .flat_map(|m| m["filenames"].as_array().cloned().unwrap_or_default())
        .filter_map(|f| f.as_str().map(PathBuf::from))
        .find(|p| p.extension().is_some_and(|e| e == "dylib" || e == "so"))
}

#[cfg(test)]
#[path = "tests/testing.rs"]
mod tests;
