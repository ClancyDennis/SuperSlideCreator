use std::path::PathBuf;
use std::sync::Arc;

use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
#[cfg(target_os = "macos")]
use tauri::TitleBarStyle;
use tokio::sync::Mutex;

mod config;
mod deck;
mod images;
mod projects;
mod provider;
mod realtime;
mod refs;
mod relay;
mod slide_agent;

/// Re-exports used by integration tests (tests/) to boot the relay router
/// standalone. Not part of the app's runtime API.
#[doc(hidden)]
pub mod test_support {
    pub use crate::images::ImageStore;
    pub use crate::refs::RefStore;
    pub use crate::relay::{build_router, AppState};
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_log::Builder::default().level(log::LevelFilter::Info).build())
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .setup(|app| {
            let web_dir = resolve_web_dir(app)?;
            let decks_dir = config::decks_dir().map_err(|e| e.to_string())?;
            let _ = std::fs::create_dir_all(&decks_dir);
            let cfg = config::load();
            let app_state = relay::AppState {
                web_dir,
                decks_dir,
                cfg: Arc::new(Mutex::new(Some(cfg))),
                app: Some(app.handle().clone()),
            };

            let (port_tx, port_rx) = std::sync::mpsc::channel::<u16>();
            let app_state_for_server = app_state.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .expect("tokio runtime");
                rt.block_on(async move {
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                        .await
                        .expect("bind 127.0.0.1:0");
                    let addr = listener.local_addr().expect("local_addr");
                    let _ = port_tx.send(addr.port());
                    let router = relay::build_router(app_state_for_server).await;
                    if let Err(e) = axum::serve(listener, router).await {
                        log::error!("axum serve error: {e}");
                    }
                });
            });

            let port = port_rx.recv().expect("relay port");
            let url = format!("http://127.0.0.1:{port}/");
            log::info!("Slide Creator relay listening on {url}");

            let handle = app.handle().clone();
            let mut builder = WebviewWindowBuilder::new(
                &handle,
                "main",
                WebviewUrl::External(url.parse().expect("url parse")),
            )
            .title("Slide Creator")
            .inner_size(1400.0, 900.0)
            .min_inner_size(900.0, 600.0);

            #[cfg(target_os = "macos")]
            {
                // Keep the traffic lights but hide the OS title bar so the
                // topbar can sit flush. The frontend reserves space on the
                // left for the lights via padding-left on the topbar.
                builder = builder
                    .title_bar_style(TitleBarStyle::Overlay)
                    .hidden_title(true);
            }
            #[cfg(not(target_os = "macos"))]
            {
                // No native frame on Win/Linux; the topbar IS the title bar.
                builder = builder.decorations(false);
            }

            builder.build()?;

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn resolve_web_dir(app: &tauri::App) -> Result<PathBuf, Box<dyn std::error::Error>> {
    // In dev (cargo tauri dev) the resource bundle isn't in place; fall back to
    // the source tree so we don't have to rebuild the bundle each time.
    if cfg!(debug_assertions) {
        let cwd = std::env::current_dir()?;
        // tauri dev runs from src-tauri/, so ../web/ is the source tree.
        let dev_web = cwd.join("..").join("web");
        if dev_web.is_dir() {
            return Ok(dev_web.canonicalize()?);
        }
    }
    let res = app
        .path()
        .resource_dir()?
        .join("web");
    Ok(res)
}
