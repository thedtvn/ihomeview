//! Native iPad desk dashboard. No WebView or desktop companion.
use nsh_core::prelude::*;

pub mod dashboard;
pub mod spotify;

/// Dashboard components available to Native Shell's native runner.
#[native_module("ihomeview/native")]
fn native(registry: &mut Registry) {
    registry.register::<App>();
}

/// The native application root.
#[component]
pub fn App() -> impl IntoElement {
    dashboard::dashboard()
}
