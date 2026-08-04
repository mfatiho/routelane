use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopApp {
    pub desktop_id: String,
    pub display_name: String,
    pub executable_path: PathBuf,
}

impl DesktopApp {
    pub fn new(
        desktop_id: impl Into<String>,
        display_name: impl Into<String>,
        executable_path: PathBuf,
    ) -> Self {
        Self {
            desktop_id: desktop_id.into(),
            display_name: display_name.into(),
            executable_path,
        }
    }
}

#[derive(Debug)]
pub enum DesktopError {
    ReadDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    ReadEntry {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl fmt::Display for DesktopError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadDirectory { path, source } => {
                write!(
                    formatter,
                    "could not read desktop entry directory {}: {source}",
                    path.display()
                )
            }
            Self::ReadEntry { path, source } => {
                write!(
                    formatter,
                    "could not read desktop entry {}: {source}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for DesktopError {}

pub fn discover_desktop_apps() -> Result<Vec<DesktopApp>, DesktopError> {
    let mut paths = Vec::new();
    if let Some(home) = std::env::var_os("XDG_DATA_HOME") {
        paths.push(PathBuf::from(home).join("applications"));
    } else if let Some(home) = std::env::var_os("HOME") {
        paths.push(PathBuf::from(home).join(".local/share/applications"));
    }
    paths.extend([
        PathBuf::from("/usr/local/share/applications"),
        PathBuf::from("/usr/share/applications"),
    ]);
    discover_from_paths(&paths)
}

pub fn discover_from_paths(paths: &[PathBuf]) -> Result<Vec<DesktopApp>, DesktopError> {
    let mut applications = BTreeMap::new();
    for path in paths {
        if !path.exists() {
            continue;
        }
        let entries = fs::read_dir(path).map_err(|source| DesktopError::ReadDirectory {
            path: path.clone(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| DesktopError::ReadDirectory {
                path: path.clone(),
                source,
            })?;
            let entry_path = entry.path();
            if entry_path.extension().and_then(|value| value.to_str()) != Some("desktop") {
                continue;
            }
            let content =
                fs::read_to_string(&entry_path).map_err(|source| DesktopError::ReadEntry {
                    path: entry_path.clone(),
                    source,
                })?;
            if let Some(application) = parse_desktop_entry(&entry_path, &content) {
                applications
                    .entry(application.desktop_id.clone())
                    .or_insert(application);
            }
        }
    }
    let mut applications = applications.into_values().collect::<Vec<_>>();
    applications.sort_by(|left, right| {
        left.display_name
            .to_lowercase()
            .cmp(&right.display_name.to_lowercase())
            .then_with(|| left.desktop_id.cmp(&right.desktop_id))
    });
    Ok(applications)
}

fn parse_desktop_entry(path: &Path, content: &str) -> Option<DesktopApp> {
    let values = desktop_entry_values(content);
    if *values.get("Type")? != "Application"
        || values.get("Hidden").is_some_and(|value| *value == "true")
        || values
            .get("NoDisplay")
            .is_some_and(|value| *value == "true")
    {
        return None;
    }
    let display_name = values.get("Name")?.trim();
    let executable_path = fs::canonicalize(executable_from_exec(values.get("Exec")?)?).ok()?;
    let desktop_id = path.file_stem()?.to_str()?.to_owned();
    (!display_name.is_empty()).then(|| DesktopApp::new(desktop_id, display_name, executable_path))
}

fn desktop_entry_values(content: &str) -> BTreeMap<&str, &str> {
    let mut values = BTreeMap::new();
    let mut in_desktop_entry = false;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_desktop_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_desktop_entry || line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            values.insert(key.trim(), value.trim());
        }
    }
    values
}

fn executable_from_exec(exec: &str) -> Option<&str> {
    let executable = exec.split_whitespace().next()?;
    (!executable.is_empty()
        && executable.starts_with('/')
        && !executable
            .bytes()
            .any(|byte| matches!(byte, b'\'' | b'"' | b'`' | b'$' | b';' | b'|' | b'&')))
    .then_some(executable)
}

#[cfg(test)]
mod tests {
    use super::{discover_from_paths, DesktopApp};
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn discovery_ignores_hidden_entries_and_extracts_exec_path() {
        let directory = temporary_desktop_directory();
        let executable = directory.join("browser-bin");
        fs::write(&executable, "").unwrap();
        let launcher = directory.join("browser-link");
        std::os::unix::fs::symlink(&executable, &launcher).unwrap();
        let executable = fs::canonicalize(&executable).unwrap();
        fs::write(
            directory.join("browser.desktop"),
            format!(
                "[Desktop Entry]\nType=Application\nName=Browser\nExec={} %U\n",
                launcher.display()
            ),
        )
        .unwrap();
        fs::write(
            directory.join("hidden.desktop"),
            "[Desktop Entry]\nType=Application\nName=Hidden\nHidden=true\nExec=/usr/bin/hidden\n",
        )
        .unwrap();

        assert_eq!(
            discover_from_paths(std::slice::from_ref(&directory)).unwrap(),
            vec![DesktopApp::new("browser", "Browser", executable,)]
        );

        fs::remove_dir_all(directory).unwrap();
    }

    fn temporary_desktop_directory() -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("routelane-desktop-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }
}
