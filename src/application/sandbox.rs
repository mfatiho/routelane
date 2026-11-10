use std::path::Path;

/// Shared sandbox launchers cannot identify a single host application.
pub fn is_sandbox_launcher(program: &Path) -> bool {
    fn launcher_name(path: &Path) -> bool {
        matches!(
            path.file_name().and_then(|name| name.to_str()),
            Some("snap" | "snapd" | "flatpak")
        )
    }
    launcher_name(program)
        || std::fs::canonicalize(program).is_ok_and(|resolved| {
            launcher_name(&resolved)
                || ["/usr/lib/snapd", "/snap", "/var/lib/flatpak"]
                    .iter()
                    .any(|prefix| resolved.starts_with(prefix))
        })
}
