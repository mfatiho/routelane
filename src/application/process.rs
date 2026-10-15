#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::domain::ApplicationSelector;

    use super::{matching_from_snapshot, RunningProcess};

    #[test]
    fn application_children_with_different_executables_follow_the_parent() {
        let mut selector = ApplicationSelector {
            desktop_id: None,
            executable_path: "/usr/bin/browser".into(),
            include_children: true,
            launch: None,
        };
        let parent = RunningProcess::new(42, 100, "/usr/bin/browser".into());
        let mut child = RunningProcess::new(43, 101, "/usr/bin/renderer".into());
        child.parent_pid = Some(42);
        let mut grandchild = RunningProcess::new(44, 102, "/usr/bin/network".into());
        grandchild.parent_pid = Some(43);
        let snapshot = vec![grandchild.clone(), child.clone(), parent.clone()];
        let matched = matching_from_snapshot(&selector, &snapshot);
        assert_eq!(
            matched
                .iter()
                .map(|process| process.pid)
                .collect::<Vec<_>>(),
            [42, 43, 44]
        );
        selector.include_children = false;
        assert_eq!(matching_from_snapshot(&selector, &snapshot), [parent]);
    }

    #[test]
    fn matching_requires_the_same_executable_path() {
        let selector = ApplicationSelector {
            desktop_id: None,
            executable_path: PathBuf::from("/usr/bin/browser"),
            include_children: true,
            launch: None,
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
    pub parent_pid: Option<u32>,
}

impl RunningProcess {
    pub fn new(pid: u32, start_time: u64, executable_path: PathBuf) -> Self {
        Self {
            pid,
            start_time,
            executable_path,
            parent_pid: None,
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
    let executable = fs::canonicalize(&selector.executable_path)
        .unwrap_or_else(|_| selector.executable_path.clone());
    let mut selected = processes
        .iter()
        .filter(|process| process.executable_path == executable)
        .cloned()
        .collect::<Vec<_>>();
    if selector.include_children {
        loop {
            let children = processes
                .iter()
                .filter(|process| {
                    !selected.iter().any(|known| known.pid == process.pid)
                        && selected.iter().any(|parent| {
                            process.parent_pid == Some(parent.pid)
                                && process.start_time >= parent.start_time
                        })
                })
                .cloned()
                .collect::<Vec<_>>();
            if children.is_empty() {
                break;
            }
            selected.extend(children);
        }
    }
    selected
}

fn process_from_proc(pid: u32) -> Option<RunningProcess> {
    let base = Path::new("/proc").join(pid.to_string());
    let executable_path = fs::read_link(base.join("exe")).ok()?;
    let stat = fs::read_to_string(base.join("stat")).ok()?;
    let fields = stat
        .rsplit_once(") ")?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    let parent_pid = fields.get(1)?.parse().ok()?;
    let start_time = fields.get(19)?.parse().ok()?;
    Some(RunningProcess {
        pid,
        start_time,
        executable_path,
        parent_pid: Some(parent_pid),
    })
}
