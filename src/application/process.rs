#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::domain::ApplicationSelector;

    use super::{matching_from_snapshot, RunningProcess};

    #[test]
    fn matching_requires_the_same_executable_path() {
        let selector = ApplicationSelector {
            desktop_id: None,
            executable_path: PathBuf::from("/usr/bin/browser"),
            include_children: true,
        };
        let processes = vec![
            RunningProcess::new(42, 101, PathBuf::from("/usr/bin/browser")),
            RunningProcess::new(43, 102, PathBuf::from("/usr/bin/other")),
        ];

        assert_eq!(
            matching_from_snapshot(&selector, &processes),
            vec![RunningProcess::new(
                42,
                101,
                PathBuf::from("/usr/bin/browser")
            )]
        );
    }
}
use std::fs;
use std::path::{Path, PathBuf};

use crate::domain::ApplicationSelector;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningProcess {
    pub pid: u32,
    pub start_time: u64,
    pub executable_path: PathBuf,
}

impl RunningProcess {
    pub fn new(pid: u32, start_time: u64, executable_path: PathBuf) -> Self {
        Self {
            pid,
            start_time,
            executable_path,
        }
    }
}

pub fn matching_processes(selector: &ApplicationSelector) -> Vec<RunningProcess> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    let snapshot = entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter_map(process_from_proc)
        .collect::<Vec<_>>();
    matching_from_snapshot(selector, &snapshot)
}

pub fn matching_from_snapshot(
    selector: &ApplicationSelector,
    processes: &[RunningProcess],
) -> Vec<RunningProcess> {
    processes
        .iter()
        .filter(|process| process.executable_path == selector.executable_path)
        .cloned()
        .collect()
}

fn process_from_proc(pid: u32) -> Option<RunningProcess> {
    let base = Path::new("/proc").join(pid.to_string());
    let executable_path = fs::read_link(base.join("exe")).ok()?;
    let stat = fs::read_to_string(base.join("stat")).ok()?;
    let start_time = stat
        .rsplit_once(") ")?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()?;
    Some(RunningProcess::new(pid, start_time, executable_path))
}
