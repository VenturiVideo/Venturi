//! Probe, decode, frame cache, encode, proxy and waveform (see
//! ARCHITECTURE.md). Proxy and waveform are on-disk caches keyed by
//! `content_hash`.

pub mod audio;
pub mod audio_file;
pub mod cache;
pub mod decode;
pub mod encode;
pub mod hw;
pub mod probe;
pub mod proxy;
pub mod thumbnail;
pub mod waveform;

pub use audio::{
    AudioBuffer, decode_audio_streams_from, decode_audio_streams_streaming, decode_audio_track,
};
pub use cache::{Served, SharedFrameCache, WantedRange};
pub use decode::{Chroma, ColorMatrix, Decoder, FrameYuv420, yuv420_frame_bytes};
pub use encode::{AudioCodec, AudioSettings, Encoder, VideoCodec, VideoSettings};
pub use hw::{HwDevice, HwPriority};
pub use probe::{
    AUDIO_EXTENSIONS, AUDIO_ONLY_FPS, AudioStreamInfo, IMAGE_EXTENSIONS, IMAGE_FPS,
    VIDEO_EXTENSIONS, audio_streams, content_fingerprint, is_image_path, is_media_path, probe,
    probe_file_info, probe_image, probe_media, probe_tags,
};
pub use thumbnail::{Thumbnail, generate_thumbnail};
pub use waveform::{
    Waveform, generate_waveforms, load_waveform, recommended_num_peaks, waveform_exists,
    waveform_path_for,
};

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("ffmpeg error: {0}")]
    Ffmpeg(#[from] ffmpeg_next::Error),
    #[error("no video/audio stream found in {0}")]
    NoStream(String),
    #[error("operation cancelled")]
    Cancelled,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Global cache directory `$XDG_CACHE_HOME/venturi/<name>` (or
/// `~/.cache/...`, or the system temp dir without `HOME`).
pub fn cache_dir(name: &str) -> std::path::PathBuf {
    use std::path::PathBuf;
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("venturi").join(name)
}

/// Test fixture (vv-app's too): generates `output` with the ffmpeg CLI.
#[doc(hidden)]
pub mod test_support {
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

    pub fn ffmpeg(args: &[&str], output: &std::path::Path) {
        let status = std::process::Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error"])
            .args(args)
            .arg(output)
            .status()
            .expect("ffmpeg CLI not found");
        assert!(status.success(), "ffmpeg {args:?} failed");
    }
}
