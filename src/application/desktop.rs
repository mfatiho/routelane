use crate::domain::ApplicationLaunch;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopApp {
    pub desktop_id: String,
    pub display_name: String,
    pub executable_path: PathBuf,
    pub launch: ApplicationLaunch,
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
            launch: ApplicationLaunch {
                executable: executable_path.clone(),
                arguments: vec![],
            },
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
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")));
    let data_dirs = std::env::var_os("XDG_DATA_DIRS");
    let paths = desktop_application_paths(data_home, data_dirs.as_deref());
    discover_from_paths(&paths)
}

fn desktop_application_paths(
    data_home: Option<PathBuf>,
    data_dirs: Option<&OsStr>,
) -> Vec<PathBuf> {
    let mut paths = data_home
        .into_iter()
        .map(|path| path.join("applications"))
        .collect::<Vec<_>>();
    let data_dirs = data_dirs.filter(|value| !value.is_empty());
    let system_paths = data_dirs
        .map(std::env::split_paths)
        .unwrap_or_else(|| std::env::split_paths(OsStr::new("/usr/local/share:/usr/share")));
    paths.extend(system_paths.map(|path| path.join("applications")));
    paths
}

pub fn discover_from_paths(paths: &[PathBuf]) -> Result<Vec<DesktopApp>, DesktopError> {
    let search_path = std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .unwrap_or_default();
    discover_from_paths_with_search_path(paths, &search_path)
}

fn discover_from_paths_with_search_path(
    paths: &[PathBuf],
    search_path: &[PathBuf],
) -> Result<Vec<DesktopApp>, DesktopError> {
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
            if let Some(application) = parse_desktop_entry(&entry_path, &content, search_path) {
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

fn parse_desktop_entry(path: &Path, content: &str, search_path: &[PathBuf]) -> Option<DesktopApp> {
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
    let command = glib::shell_parse_argv(values.get("Exec")?).ok()?;
    let launcher = resolve_executable(command.first()?.to_str()?, search_path)?;
    let arguments = command
        .iter()
        .skip(1)
        .filter_map(|argument| {
            let argument = argument.to_str()?;
            match argument {
                "%u" | "%U" | "%f" | "%F" | "%i" => None,
                "%c" => Some(display_name.to_owned()),
                "%k" => Some(path.to_string_lossy().into_owned()),
                _ => Some(argument.replace("%%", "%")),
            }
        })
        .collect::<Vec<_>>();
    let executable_path = fs::canonicalize(&launcher).ok()?;
    let desktop_id = path.file_stem()?.to_str()?.to_owned();
    (!display_name.is_empty()).then(|| DesktopApp {
        desktop_id,
        display_name: display_name.to_owned(),
        executable_path,
        launch: ApplicationLaunch {
            executable: launcher,
            arguments,
        },
    })
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

fn resolve_executable(executable: &str, search_path: &[PathBuf]) -> Option<PathBuf> {
    let path = Path::new(executable);
    if path.is_absolute() {
        return path.is_file().then(|| path.to_path_buf());
    }
    if path.components().count() != 1 {
        return None;
    }
    search_path
        .iter()
        .map(|directory| directory.join(path))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::{
        desktop_application_paths, discover_from_paths, discover_from_paths_with_search_path,
    };
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn discovery_preserves_launcher_alias_and_quoted_arguments() {
        let directory = temporary_desktop_directory();
        let executable = directory.join("browser-bin");
        fs::write(&executable, "").unwrap();
        let launcher = directory.join("browser-link");
        std::os::unix::fs::symlink(&executable, &launcher).unwrap();
        fs::write(directory.join("browser.desktop"), format!(
            "[Desktop Entry]\nType=Application\nName=Browser\nExec=\"{}\" --profile \"Work Profile\" %U\n", launcher.display()
        )).unwrap();
        let apps = discover_from_paths(std::slice::from_ref(&directory)).unwrap();
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].launch.executable, launcher);
        assert_eq!(apps[0].launch.arguments, ["--profile", "Work Profile"]);
        assert_eq!(
            apps[0].executable_path,
            fs::canonicalize(executable).unwrap()
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn desktop_paths_include_every_xdg_data_directory() {
        assert_eq!(
            desktop_application_paths(
                Some(PathBuf::from("/home/test/.local/share")),
                Some(std::ffi::OsStr::new(
                    "/usr/local/share:/usr/share:/var/lib/snapd/desktop",
                )),
            ),
            vec![
                PathBuf::from("/home/test/.local/share/applications"),
                PathBuf::from("/usr/local/share/applications"),
                PathBuf::from("/usr/share/applications"),
                PathBuf::from("/var/lib/snapd/desktop/applications"),
            ]
        );
    }

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

        let apps = discover_from_paths(std::slice::from_ref(&directory)).unwrap();
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].desktop_id, "browser");
        assert_eq!(apps[0].display_name, "Browser");
        assert_eq!(apps[0].executable_path, executable);
        assert_eq!(apps[0].launch.executable, launcher);

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn discovery_resolves_a_bare_exec_name_from_the_search_path() {
        let directory = temporary_desktop_directory();
        let bin = directory.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let executable = bin.join("browser");
        fs::write(&executable, "").unwrap();
        fs::write(
            directory.join("browser.desktop"),
            "[Desktop Entry]\nType=Application\nName=Browser\nExec=browser %U\n",
        )
        .unwrap();

        assert_eq!(
            discover_from_paths_with_search_path(
                std::slice::from_ref(&directory),
                std::slice::from_ref(&bin),
            )
            .unwrap()[0]
                .executable_path,
            fs::canonicalize(executable).unwrap()
        );

        fs::remove_dir_all(directory).unwrap();
    }

    fn temporary_desktop_directory() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "routelane-desktop-test-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }
}
