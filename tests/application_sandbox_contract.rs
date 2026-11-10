use routelane::application::is_sandbox_launcher;
use std::os::unix::fs::symlink;
use std::path::Path;

#[test]
fn symlinked_snap_launcher_is_detected() {
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path();
    std::fs::create_dir_all(dir.join("lib/snapd")).unwrap();
    std::fs::create_dir(dir.join("bin")).unwrap();
    std::fs::write(dir.join("lib/snapd/snapd"), b"launcher").unwrap();
    symlink("../lib/snapd/snapd", dir.join("bin/snap")).unwrap();
    symlink("snap", dir.join("bin/firefox")).unwrap();
    assert!(is_sandbox_launcher(&dir.join("bin/snap")));
    assert!(is_sandbox_launcher(&dir.join("bin/firefox")));
    assert!(is_sandbox_launcher(Path::new(
        "/usr/bin/flatpak-missing/../flatpak"
    )));
    assert!(is_sandbox_launcher(Path::new("/opt/x/snap")));
    std::fs::write(dir.join("bin/flatpak"), b"launcher").unwrap();
    symlink("flatpak", dir.join("bin/wrapper")).unwrap();
    assert!(is_sandbox_launcher(&dir.join("bin/wrapper")));
}

#[test]
fn host_applications_are_not_sandbox_launchers() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("bin")).unwrap();
    for name in ["browser", "snapshot-tool"] {
        let path = dir.path().join("bin").join(name);
        std::fs::write(&path, b"host").unwrap();
        assert!(!is_sandbox_launcher(&path));
    }
}
