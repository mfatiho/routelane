mod desktop;
mod process;

pub use desktop::{discover_desktop_apps, discover_from_paths, DesktopApp, DesktopError};
pub use process::{matching_from_snapshot, matching_processes, RunningProcess};
