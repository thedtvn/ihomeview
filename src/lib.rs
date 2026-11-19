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
    // Keep the screen on per the setting (default: only while charging). Re-checked so plugging
    // in or unplugging takes effect within half a minute.
    keep_awake::apply();
    set_interval(std::time::Duration::from_secs(30), keep_awake::apply);
    dashboard::dashboard()
}
