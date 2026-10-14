use gtk4::{gio, prelude::*};
use routelane::application::{discover_desktop_apps, DesktopApp};
use routelane::domain::ApplicationSelector;

#[derive(Debug, Clone)]
pub struct ApplicationChoice {
    pub app: DesktopApp,
    pub icon_name: Option<String>,
    pub unsupported_reason: Option<&'static str>,
}
#[derive(Debug, Clone, Default)]
pub struct DesktopCatalog {
    choices: Vec<ApplicationChoice>,
}

impl DesktopCatalog {
    pub fn load() -> Result<Self, String> {
        discover_desktop_apps()
            .map(Self::from_apps)
            .map_err(|error| error.to_string())
    }
    pub fn from_apps(apps: Vec<DesktopApp>) -> Self {
        let choices = apps
            .into_iter()
            .map(|mut app| {
                let info = gio::DesktopAppInfo::new(&format!("{}.desktop", app.desktop_id));
                let icon_name = info
                    .as_ref()
                    .and_then(|info| info.icon())
                    .and_then(|icon| icon.downcast::<gio::ThemedIcon>().ok())
                    .and_then(|icon| icon.names().first().map(|name| name.to_string()));
                if let Some(info) = info {
                    app.display_name = info.display_name().to_string();
                }
                let unsupported_reason = [&app.executable_path, &app.launch.executable]
                    .into_iter()
                    .any(|program| {
                        let resolved =
                            std::fs::canonicalize(program).unwrap_or_else(|_| program.clone());
                        matches!(
                            resolved.file_name().and_then(|name| name.to_str()),
                            Some("snap" | "flatpak")
                        )
                    })
                    .then_some("sandbox");
                ApplicationChoice {
                    app,
                    icon_name,
                    unsupported_reason,
                }
            })
            .collect();
        Self { choices }
    }
    pub fn choices(&self) -> &[ApplicationChoice] {
        &self.choices
    }
    pub fn find(&self, selector: &ApplicationSelector) -> Option<&ApplicationChoice> {
        self.choices.iter().find(|choice| {
            choice.app.executable_path == selector.executable_path
                && selector
                    .desktop_id
                    .as_ref()
                    .is_none_or(|id| id == &choice.app.desktop_id)
        })
    }
}

pub fn application_display_name(
    selector: &ApplicationSelector,
    catalog: &DesktopCatalog,
) -> String {
    catalog
        .find(selector)
        .map(|choice| choice.app.display_name.clone())
        .unwrap_or_else(|| {
            selector
                .executable_path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
                .unwrap_or_else(|| selector.executable_path.to_string_lossy().into_owned())
        })
}

pub fn load_catalog(callback: impl FnOnce(Result<DesktopCatalog, String>) + 'static) {
    let (sender, receiver) = async_channel::bounded(1);
    std::thread::spawn(move || {
        sender.send_blocking(DesktopCatalog::load()).ok();
    });
    glib::MainContext::ref_thread_default().spawn_local(async move {
        if let Ok(result) = receiver.recv().await {
            callback(result);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_use_desktop_metadata_then_executable_fallback() {
        let catalog = DesktopCatalog::from_apps(vec![DesktopApp::new(
            "routelane-test-browser",
            "Friendly Browser",
            "/opt/browser".into(),
        )]);
        let selector = ApplicationSelector {
            desktop_id: Some("routelane-test-browser".into()),
            executable_path: "/opt/browser".into(),
            include_children: true,
            launch: None,
        };
        assert_eq!(
            application_display_name(&selector, &catalog),
            "Friendly Browser"
        );
        let missing = DesktopCatalog::from_apps(vec![]);
        assert_eq!(application_display_name(&selector, &missing), "browser");
    }
    #[test]
    fn sandbox_launcher_is_not_selectable_as_a_host_application() {
        let catalog = DesktopCatalog::from_apps(vec![DesktopApp::new(
            "routelane-test-snap",
            "Sandbox Browser",
            "/usr/bin/snap".into(),
        )]);
        assert_eq!(catalog.choices()[0].unsupported_reason, Some("sandbox"));
    }
    #[test]
    fn stale_desktop_identity_does_not_match_another_program() {
        let catalog = DesktopCatalog::from_apps(vec![DesktopApp::new(
            "other-app",
            "Other App",
            "/usr/bin/shared".into(),
        )]);
        let selector = ApplicationSelector {
            desktop_id: Some("original-app".into()),
            executable_path: "/usr/bin/shared".into(),
            include_children: true,
            launch: None,
        };
        assert!(catalog.find(&selector).is_none());
    }
}
