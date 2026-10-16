//! EchoBird's local Responses endpoint for Muse Code.

pub(crate) mod convert;
#[cfg(test)]
mod native_test;
mod server;

use std::sync::Mutex;

const DEFAULT_PORT: u16 = 53684;
static RUNNING_PORT: Mutex<Option<u16>> = Mutex::new(None);

pub(crate) fn port() -> Result<u16, String> {
    RUNNING_PORT
        .lock()
        .map_err(|error| error.to_string())?
        .map(Ok)
        .unwrap_or_else(|| super::local_proxy::saved_port("musecode-proxy", DEFAULT_PORT))
}

/// Bind before publishing the address or writing a client's configuration.
pub(crate) fn ensure_available() -> Result<u16, String> {
    let mut running = RUNNING_PORT.lock().map_err(|e| e.to_string())?;
    if let Some(port) = *running {
        return Ok(port);
    }
    let app = server::router(crate::utils::platform::echobird_dir().join("musecode.json"))?;
    let listener = super::local_proxy::bind("musecode-proxy", DEFAULT_PORT)?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    *running = Some(port);
    tauri::async_runtime::spawn(async move {
        if let Err(error) = server::run(listener, app).await {
            log::error!("[MuseCodeProxy] {error}");
        }
        if let Ok(mut running) = RUNNING_PORT.lock() {
            *running = None;
        }
    });
    Ok(port)
}

pub fn spawn_proxy_task() {
    if let Err(error) = ensure_available() {
        log::error!("[MuseCodeProxy] {error}");
    }
}
