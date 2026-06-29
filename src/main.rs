mod config;
mod models;
mod routing;
mod ui;

use crate::models::{EngineToUi, UiToEngine};
use async_channel::bounded;
use libadwaita::{self as adw, prelude::*};

// ──────────────────────────────────────────────────────────────────────────────
// Giriş noktası
//
// Mimari:
//   ┌──────────────────┐  UiToEngine   ┌────────────────────────┐
//   │  GTK main loop   │ ─────────────>│  Tokio (thread pool)   │
//   │  (main thread)   │ <─────────────│  routing::engine_main  │
//   └──────────────────┘  EngineToUi  └────────────────────────┘
//
// async-channel hem GTK tarafında (glib::MainContext::spawn_local) hem de
// Tokio tarafında await ile kullanılabilir.
// ──────────────────────────────────────────────────────────────────────────────

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let (ui_tx, ui_rx) = bounded::<UiToEngine>(64);
    let (engine_tx, engine_rx) = bounded::<EngineToUi>(64);

    // Tokio runtime — GTK main loop'tan ayrı bir OS thread'inde çalışır
    let rt = tokio::runtime::Runtime::new().expect("Tokio runtime oluşturulamadı");
    std::thread::spawn(move || {
        rt.block_on(routing::engine_main(ui_rx, engine_tx));
    });

    let app = adw::Application::builder()
        .application_id(ui::tray::APP_ID)
        .build();

    // connect_activate: ana pencereyi oluştur
    {
        let ui_tx = ui_tx.clone();
        app.connect_activate(move |app| {
            ui::window::build_window(app, ui_tx.clone(), engine_rx.clone());
        });
    }

    // connect_shutdown: engine'e kapanış sinyali gönder
    // send_blocking yerine try_send kullanılır (sync bağlam); kanal doluysa yok say.
    // Engine zaten kanal kapanınca temizlik yapıyor, bu mesaj sadece destek amaçlı.
    {
        let ui_tx = ui_tx.clone();
        app.connect_shutdown(move |_| {
            ui_tx.try_send(UiToEngine::Shutdown).ok();
        });
    }

    app.run();
}
