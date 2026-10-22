//! The application core without any UI: what a document session needs to
//! import, relink and export, shared by the GUI and headless hosts.

pub mod analysis;
pub mod audio_render;
pub mod export;
pub mod forced_relink;
pub mod frame_provider;
pub mod import_worker;
mod jobs;
mod media_tree;
pub mod relink_job;
mod session;
pub mod silence;
pub mod worker;

pub use jobs::{JobId, OtioMerged, RelinkEnd, SessionEvent, Waker, file_label};
pub use media_tree::MediaTree;
pub use session::{ChangeMark, Session, complete_legacy_media};

#[cfg(test)]
mod test_support {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Unique per-test directory, removed on drop.
    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new(prefix: &str) -> Self {
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "{prefix}-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }

        pub fn join(&self, name: impl AsRef<Path>) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
