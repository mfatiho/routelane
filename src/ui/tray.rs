use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::Duration;

use crate::config::Language;
use glib::Sender;
use ksni::blocking::TrayMethods;

pub const APP_ID: &str = "io.github.routelane";
pub const APP_ICON_NAME: &str = "network-workgroup-symbolic";

#[derive(Debug, Clone, Copy)]
pub enum TrayCommand {
    ShowWindow,
    Quit,
}

struct RouteLaneTray {
    sender: Sender<TrayCommand>,
    language: Arc<Mutex<Language>>,
}

impl RouteLaneTray {
    fn language(&self) -> Language {
        self.language
            .lock()
            .map(|language| *language)
            .unwrap_or_default()
    }
}

impl ksni::Tray for RouteLaneTray {
    fn id(&self) -> String {
        APP_ID.to_owned()
    }

    fn title(&self) -> String {
        "RouteLane".to_owned()
    }

    fn icon_name(&self) -> String {
        APP_ICON_NAME.to_owned()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let texts = self.language().texts();
        ksni::ToolTip {
            title: "RouteLane".to_owned(),
            description: texts.tray_tooltip.to_owned(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.sender.send(TrayCommand::ShowWindow);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;

        let texts = self.language().texts();

        vec![
            StandardItem {
                label: "RouteLane".to_owned(),
                enabled: false,
                icon_name: APP_ICON_NAME.to_owned(),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: texts.tray_description.to_owned(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: texts.tray_show.to_owned(),
                icon_name: "window-new-symbolic".to_owned(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.sender.send(TrayCommand::ShowWindow);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: texts.tray_quit.to_owned(),
                icon_name: "application-exit-symbolic".to_owned(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.sender.send(TrayCommand::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

pub fn spawn_tray(sender: Sender<TrayCommand>, language: Arc<Mutex<Language>>) -> Arc<AtomicBool> {
    let is_available = Arc::new(AtomicBool::new(false));
    let is_available_for_thread = is_available.clone();

    thread::spawn(move || {
        let tray = RouteLaneTray { sender, language };
        match tray.spawn() {
            Ok(handle) => {
                is_available_for_thread.store(true, Ordering::Relaxed);
                while !handle.is_closed() {
                    thread::sleep(Duration::from_secs(60));
                }
                is_available_for_thread.store(false, Ordering::Relaxed);
            }
            Err(err) => {
                log::warn!("Tray icon başlatılamadı: {}", err);
            }
        }
    });

    is_available
}
