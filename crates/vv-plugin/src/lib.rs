mod module;
mod scan;
#[cfg(any(test, feature = "testing"))]
#[doc(hidden)]
pub mod testing;

pub use module::bundle_binary;
pub use scan::{PluginInfo, abandon_scan, default_dirs, scan, scan_with_guard, take_crashed};
mod editor;
mod host;
mod installed;
mod instance;
mod instances;
pub use editor::Editor;
pub use host::{Waker, set_restart_waker};
pub use installed::{installed, same_plugins, scanning, set_installed, set_scanning};
pub use instance::{ProcessMode, Vst3Instance};
pub use instances::{HostedPlugin, Instances, MakeInstance};
