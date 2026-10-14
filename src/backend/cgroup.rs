use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::backend::{CompiledRule, CompiledTarget};
use crate::domain::RuleId;

const APPLICATION_CGROUP_ROOT: &str = "/sys/fs/cgroup/routelane.apps";

pub fn application_cgroup_rule_ids(rules: &[CompiledRule]) -> Result<Vec<RuleId>, CgroupError> {
    let mut ids = Vec::new();
    for rule in rules {
        let CompiledTarget::Cgroup(path) = &rule.target else {
            continue;
        };
        let Some(id) = path.strip_prefix("routelane.apps/") else {
            return Err(CgroupError::InvalidRuleId);
        };
        if id != rule.id.as_str() {
            return Err(CgroupError::InvalidRuleId);
        }
        ids.push(rule.id.clone());
    }
    Ok(ids)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub start_time: u64,
    pub executable_path: PathBuf,
}

impl ProcessIdentity {
    pub fn new(pid: u32, start_time: u64, executable_path: PathBuf) -> Self {
        Self {
            pid,
            start_time,
            executable_path,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CgroupError {
    InvalidRuleId,
    ProcessIdentityMismatch { pid: u32 },
    CgroupNotEmpty { rule_id: RuleId },
    Io(String),
}

impl fmt::Display for CgroupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRuleId => {
                formatter.write_str("application rule ID is unsafe for cgroup use")
            }
            Self::ProcessIdentityMismatch { pid } => {
                write!(
                    formatter,
                    "process {pid} no longer matches the requested application"
                )
            }
            Self::CgroupNotEmpty { rule_id } => {
                write!(
                    formatter,
                    "application cgroup for {} is not empty",
                    rule_id.as_str()
                )
            }
            Self::Io(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for CgroupError {}

#[derive(Debug, Clone)]
pub struct ApplicationCgroupManager {
    root: PathBuf,
    proc_root: PathBuf,
    create_control_files: bool,
    delegate_to_owner: bool,
}

impl ApplicationCgroupManager {
    pub fn system() -> Self {
        Self {
            root: PathBuf::from(APPLICATION_CGROUP_ROOT),
            proc_root: PathBuf::from("/proc"),
            create_control_files: false,
            delegate_to_owner: true,
        }
    }

    #[cfg(test)]
    fn for_test(root: PathBuf, proc_root: PathBuf) -> Self {
        Self {
            root,
            proc_root,
            create_control_files: true,
            delegate_to_owner: false,
        }
    }

    pub fn prepare(&self, rule_id: &RuleId, owner_uid: u32) -> Result<PathBuf, CgroupError> {
        let path = self.rule_path(rule_id)?;
        fs::create_dir_all(&path).map_err(io_error)?;
        if self.create_control_files {
            fs::write(path.join("cgroup.procs"), "").map_err(io_error)?;
        }
        if self.delegate_to_owner {
            for target in delegation_targets(&path) {
                chown(&target, owner_uid)?;
            }
        }
        Ok(path)
    }

    pub fn adopt(&self, rule_id: &RuleId, identity: &ProcessIdentity) -> Result<(), CgroupError> {
        let path = self.rule_path(rule_id)?;
        if !self.matches_process(identity) {
            return Err(CgroupError::ProcessIdentityMismatch { pid: identity.pid });
        }
        let mut control = OpenOptions::new()
            .write(true)
            .open(path.join("cgroup.procs"))
            .map_err(io_error)?;
        writeln!(control, "{}", identity.pid).map_err(io_error)
    }

    pub fn cleanup(&self, rule_id: &RuleId) -> Result<(), CgroupError> {
        let path = self.rule_path(rule_id)?;
        if !path.exists() {
            return Ok(());
        }
        let members = fs::read_to_string(path.join("cgroup.procs")).map_err(io_error)?;
        if !members.trim().is_empty() {
            return Err(CgroupError::CgroupNotEmpty {
                rule_id: rule_id.clone(),
            });
        }
        fs::remove_dir(&path).map_err(io_error)
    }

    pub fn verified_orphan_directories(&self) -> Result<Vec<PathBuf>, CgroupError> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let mut paths = Vec::new();
        for entry in fs::read_dir(&self.root).map_err(io_error)? {
            let entry = entry.map_err(io_error)?;
            let file_type = entry.file_type().map_err(io_error)?;
            if file_type.is_symlink() {
                return Err(CgroupError::Io(
                    "RouteLane application cgroup root contains a symbolic link".to_owned(),
                ));
            }
            if file_type.is_file() {
                continue;
            }
            if !file_type.is_dir() {
                return Err(CgroupError::Io(
                    "RouteLane application cgroup root contains an unsupported entry".to_owned(),
                ));
            }
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| CgroupError::InvalidRuleId)?;
            let rule_id = RuleId::from(name.as_str());
            let path = self.rule_path(&rule_id)?;
            if path != entry.path() {
                return Err(CgroupError::InvalidRuleId);
            }
            let members = fs::read_to_string(path.join("cgroup.procs")).map_err(io_error)?;
            if !members.trim().is_empty() {
                return Err(CgroupError::CgroupNotEmpty { rule_id });
            }
            for child in fs::read_dir(&path).map_err(io_error)? {
                let child = child.map_err(io_error)?;
                if child.file_type().map_err(io_error)?.is_dir() {
                    return Err(CgroupError::Io(
                        "RouteLane application cgroup contains a nested directory".to_owned(),
                    ));
                }
            }
            paths.push(path);
        }
        paths.sort();
        Ok(paths)
    }

    pub fn remove_verified_orphan_directories(&self, paths: &[PathBuf]) -> Result<(), CgroupError> {
        let verified = self.verified_orphan_directories()?;
        if paths.iter().any(|path| !verified.contains(path)) {
            return Err(CgroupError::Io(
                "application cgroup cleanup target was not verified".to_owned(),
            ));
        }
        for path in paths {
            if self.create_control_files {
                fs::remove_file(path.join("cgroup.procs")).map_err(io_error)?;
            }
            fs::remove_dir(path).map_err(io_error)?;
        }
        Ok(())
    }

    fn rule_path(&self, rule_id: &RuleId) -> Result<PathBuf, CgroupError> {
        let name = rule_id.as_str();
        if name.is_empty()
            || name == "."
            || name == ".."
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(CgroupError::InvalidRuleId);
        }
        Ok(self.root.join(name))
    }

    fn matches_process(&self, identity: &ProcessIdentity) -> bool {
        let path = self.proc_root.join(identity.pid.to_string());
        let Ok(executable_path) = fs::read_link(path.join("exe")) else {
            return false;
        };
        if executable_path != identity.executable_path {
            return false;
        }
        let Ok(stat) = fs::read_to_string(path.join("stat")) else {
            return false;
        };
        stat.rsplit_once(") ")
            .and_then(|(_, fields)| fields.split_whitespace().nth(19))
            .and_then(|value| value.parse::<u64>().ok())
            == Some(identity.start_time)
    }
}

fn delegation_targets(path: &Path) -> [PathBuf; 2] {
    [path.to_owned(), path.join("cgroup.procs")]
}

fn chown(path: &Path, owner_uid: u32) -> Result<(), CgroupError> {
    let path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| CgroupError::Io("cgroup path contains an interior NUL byte".to_owned()))?;
    let result = unsafe { libc::chown(path.as_ptr(), owner_uid, owner_uid) };
    if result == 0 {
        Ok(())
    } else {
        Err(io_error(std::io::Error::last_os_error()))
    }
}

fn io_error(error: std::io::Error) -> CgroupError {
    CgroupError::Io(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::domain::RuleId;

    use super::{ApplicationCgroupManager, CgroupError, ProcessIdentity};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn adoption_rejects_pid_when_executable_or_start_time_differs() {
        let fixture = Fixture::new();
        fixture.process(42, 100, "/usr/bin/browser");
        let manager = fixture.manager();
        manager.prepare(&RuleId::from("browser"), 1000).unwrap();

        let error = manager
            .adopt(
                &RuleId::from("browser"),
                &ProcessIdentity::new(42, 101, PathBuf::from("/usr/bin/browser")),
            )
            .unwrap_err();

        assert_eq!(error, CgroupError::ProcessIdentityMismatch { pid: 42 });
    }

    #[test]
    fn cleanup_rejects_rule_ids_that_could_escape_the_routelane_subtree() {
        let fixture = Fixture::new();
        let manager = fixture.manager();

        assert_eq!(
            manager
                .cleanup(&RuleId::from("../../user.slice"))
                .unwrap_err(),
            CgroupError::InvalidRuleId
        );
    }

    #[test]
    fn delegation_includes_the_directory_and_its_process_control_file() {
        let path = PathBuf::from("/sys/fs/cgroup/routelane.apps/browser");

        assert_eq!(
            super::delegation_targets(&path),
            [path.clone(), path.join("cgroup.procs")]
        );
    }

    #[test]
    fn application_rule_ids_are_extracted_from_a_validated_kernel_plan() {
        let rules = vec![crate::backend::CompiledRule {
            family: crate::backend::IpFamily::V4,
            id: RuleId::from("browser"),
            priority: 1,
            target: crate::backend::CompiledTarget::Cgroup("routelane.apps/browser".to_owned()),
            action: crate::backend::CompiledAction::Mark(crate::backend::MARK_BASE),
        }];

        assert_eq!(
            super::application_cgroup_rule_ids(&rules).unwrap(),
            vec![RuleId::from("browser")]
        );
    }

    #[test]
    fn orphan_preflight_returns_only_empty_direct_route_lane_cgroups() {
        let fixture = Fixture::new();
        let manager = fixture.manager();
        let browser = manager.prepare(&RuleId::from("browser"), 1000).unwrap();

        assert_eq!(
            manager.verified_orphan_directories().unwrap(),
            vec![browser]
        );
    }

    #[test]
    fn orphan_preflight_rejects_a_symlink_before_cleanup() {
        let fixture = Fixture::new();
        let manager = fixture.manager();
        fs::create_dir_all(&manager.root).unwrap();
        std::os::unix::fs::symlink(&fixture.proc_root, manager.root.join("browser")).unwrap();

        assert!(matches!(
            manager.verified_orphan_directories(),
            Err(CgroupError::Io(_))
        ));
        assert!(manager.root.join("browser").exists());
    }

    #[test]
    fn orphan_preflight_ignores_root_cgroup_control_files() {
        let fixture = Fixture::new();
        let manager = fixture.manager();
        let browser = manager.prepare(&RuleId::from("browser"), 1000).unwrap();
        fs::write(manager.root.join("cgroup.controllers"), "cpu memory").unwrap();

        assert_eq!(
            manager.verified_orphan_directories().unwrap(),
            vec![browser]
        );
    }

    struct Fixture {
        root: PathBuf,
        proc_root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "routelane-cgroup-test-{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&root);
            let proc_root = root.join("proc");
            fs::create_dir_all(&proc_root).unwrap();
            Self { root, proc_root }
        }

        fn manager(&self) -> ApplicationCgroupManager {
            ApplicationCgroupManager::for_test(self.root.join("cgroup"), self.proc_root.clone())
        }

        fn process(&self, pid: u32, start_time: u64, executable: &str) {
            let path = self.proc_root.join(pid.to_string());
            fs::create_dir_all(&path).unwrap();
            std::os::unix::fs::symlink(executable, path.join("exe")).unwrap();
            fs::write(
                path.join("stat"),
                format!("{pid} (browser) S 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 {start_time}"),
            )
            .unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}
