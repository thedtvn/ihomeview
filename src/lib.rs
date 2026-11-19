//! Native iPad desk dashboard. No WebView or desktop companion.
use nsh_core::prelude::*;

pub mod dashboard;
pub mod debug_log;
pub mod keep_awake;
pub mod spotify;

/// Dashboard components available to Native Shell's native runner.
#[native_module("ihomeview/native")]
fn native(registry: &mut Registry) {
    registry.register::<App>();
}

/// The native application root.
#[component]
pub fn App() -> impl IntoElement {
    // Desk display: keep the screen on (iOS re-enables the idle timer in the background).
    keep_awake::keep_screen_on();
    set_interval(std::time::Duration::from_secs(60), keep_awake::keep_screen_on);
    dashboard::dashboard()
}
