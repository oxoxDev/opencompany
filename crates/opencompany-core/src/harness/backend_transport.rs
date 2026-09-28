//! The TinyHumans backend transport, installed once per process.
//!
//! At the 1ecf1b0 OpenHuman pin the core reaches the hosted TinyHumans
//! backend only through an installed [`BackendTransport`]
//! (`openhuman_core::backend::transport`), and `openhuman-embed` alone installs
//! none: managed search, Composio, media generation — every `IntegrationClient`
//! call this crate makes — answers `BACKEND_UNAVAILABLE` until a host installs
//! one. `openhuman-tinyhumans` carries the SDK-backed implementation; this
//! module is the one place it is installed, so a `serve` boot, a runtime
//! build and a unit test that builds a search tool directly all land on the
//! same transport.
//!
//! [`BackendTransport`]: openhuman_core::backend::transport::BackendTransport

use std::sync::Arc;

use openhuman_core::backend::transport::BackendTransport;

/// Installs the SDK-backed transport if none is installed yet, and returns
/// it. Idempotent and cheap after the first call.
///
/// `None` when the transport's HTTP client cannot be built — the core then
/// keeps answering `BACKEND_UNAVAILABLE`, which is the same failure the
/// callers already handle, so this never turns a tool build into an error.
pub fn ensure_installed() -> Option<Arc<dyn BackendTransport>> {
    if let Some(existing) = openhuman_core::backend::transport::installed_backend_transport() {
        return Some(existing);
    }
    let mut options = openhuman_tinyhumans::InstallOptions::default()
        // The hosted RPC proxies (`billing`, `team`, …) are the desktop
        // app's surface; this host serves none of them.
        .hosted_controllers(false);
    if let Some(identity) =
        openhuman_tinyhumans::ProductIdentity::new(crate::product::PRODUCT_IDENTITY)
    {
        options = options.product_identity(identity);
    }
    match openhuman_tinyhumans::install(options) {
        Ok(transport) => Some(transport as Arc<dyn BackendTransport>),
        Err(err) => {
            tracing::warn!(%err, "[backend-transport] could not install the TinyHumans transport");
            None
        }
    }
}
