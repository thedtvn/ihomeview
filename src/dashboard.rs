//! The ihomeview dashboard root.
//!
//! Scope of this file: native clock/date, a focus timer (presets, start/pause/resume/reset),
//! a 12/24-hour toggle and a night-dim toggle. Music and camera are shown honestly as
//! unavailable: librespot has no demonstrated remote-control path to the Windows Spotify
//! session (see docs/spotify-feasibility.md) and Imou is not configured. No fake metadata,
//! no working-looking controls for either. No WebView/TSX; pure NSH Rust native UI.
//!
//! Built against the NSH Rust docs (https://nsh.dev/docs/rust.md,
//! https://nsh.dev/docs/reactivity.md, https://nsh.dev/docs/layout.md,
//! https://nsh.dev/docs/styling.md, https://nsh.dev/docs/accessibility.md).
//! `nsh.json` wiring (`"root": "Dashboard"`) and Cargo dependencies are owned by the
//! scaffolding lane, not this file.

use std::time::Duration;

use chrono::{Datelike, Local, Timelike};
use nsh_base::theme;
use nsh_core::{Checked, prelude::*, time::unix_ms};

/// Focus timer presets, in minutes.
const PRESETS_MIN: [i64; 4] = [5, 15, 25, 45];
const PRESET_TEST_IDS: [&str; 4] = [
    "timer_preset_5",
    "timer_preset_15",
    "timer_preset_25",
    "timer_preset_45",
];
const PRESET_LABELS: [&str; 4] = [
    "5 minutes",
    "15 minutes",
    "25 minutes",
    "45 minutes",
];
const PRESET_TEXTS: [&str; 4] = ["5m", "15m", "25m", "45m"];

/// `(HH:MM:SS, "AM"/"PM" or "")`.
fn format_clock(h: u32, m: u32, s: u32, twelve_hour: bool) -> (String, &'static str) {
    if twelve_hour {
        let period = if h < 12 { "AM" } else { "PM" };
        let h12 = match h % 12 {
            0 => 12,
            other => other,
        };
        (format!("{h12:02}:{m:02}:{s:02}"), period)
    } else {
        (format!("{h:02}:{m:02}:{s:02}"), "")
    }
}

fn format_date_parts(weekday: &str, month: &str, day: u32, year: i32) -> String {
    format!("{weekday}, {month} {day}, {year}")
}

fn format_date(dt: &chrono::DateTime<Local>) -> String {
    format_date_parts(
        &dt.format("%A").to_string(),
        &dt.format("%B").to_string(),
        dt.day(),
        dt.year(),
    )
}

fn format_remaining(total_secs: i64) -> String {
    let total_secs = total_secs.max(0);
    format!("{:02}:{:02}", total_secs / 60, total_secs % 60)
}

/// The dashboard's root view.
#[component]
pub fn Dashboard() -> impl IntoElement {
    // Clock: local time ticks every 500ms so seconds stay fresh without drift.
    // The interval ends with this view, per https://nsh.dev/docs/reactivity.md#cleanup.
    let now_local = signal(Local::now());
    set_interval(Duration::from_millis(500), move || now_local.set(Local::now()));

    let twelve_hour = signal(locale_settings().hour_cycle.twelve_hour());
    let night_dim = signal(false);

    let clock_display = computed(move || {
        let dt = now_local.get();
        format_clock(dt.hour(), dt.minute(), dt.second(), twelve_hour.get())
    });
    let date_display = computed(move || {
        let dt = now_local.get();
        format_date(&dt)
    });

    // Focus timer state.
    let preset_index = signal(1usize); // 15 minutes by default.
    let remaining_secs = signal(PRESETS_MIN[1] * 60);
    let running = signal(false);

    // Wall-clock countdown: uses a target deadline so system sleep / suspensions
    // don't freeze or drift the countdown. Pausing captures the exact snapshot.
    effect(move || {
        if !running.get() {
            return;
        }
        let initial_remaining = remaining_secs.get_untracked();
        let deadline_ms = unix_ms() + (initial_remaining as f64) * 1000.0;
        let ticker = spawn(async move {
            loop {
                timer(Duration::from_millis(250)).await;
                let now = unix_ms();
                let left = ((deadline_ms - now) / 1000.0).ceil() as i64;
                if left <= 0 {
                    remaining_secs.set(0);
                    running.set(false);
                    break;
                } else {
                    remaining_secs.set(left);
                }
            }
        });
        cleanup(move || ticker.cancel());
    });

    view()
        .flex_1()
        .bg(AdaptiveColor::rgb(0x0b0b0d, 0x0b0b0d))
        .test_id("dashboard_root")
        .child(
            scroll()
                .flex_1()
                .child(
                    view()
                        .p(px(28.0))
                        .gap(px(24.0))
                        .flex_col()
                        // Landscape (the primary 1024x768 orientation): clock and timer side by side.
                        .style_when(|| window().width > window().height, |s| s.flex_row())
                        .child(
                            view()
                                .style_when(|| window().width > window().height, |s| s.flex_1())
                                .gap(px(20.0))
                                .child(header_row(twelve_hour, night_dim))
                                .child(clock_block(clock_display, date_display)),
                        )
                        .child(
                            view()
                                .style_when(|| window().width > window().height, |s| s.flex_1())
                                .gap(px(20.0))
                                .child(focus_timer_block(preset_index, remaining_secs, running))
                                .child(status_row()),
                        ),
                ),
        )
        .child(night_dim_overlay(night_dim))
}

fn header_row(twelve_hour: Signal<bool>, night_dim: Signal<bool>) -> impl IntoElement {
    view()
        .flex_row()
        .items_center()
        .justify_between()
        .child(
            text("ihomeview")
                .role(AccessibilityRole::Header)
                .text_size(px(18.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(AdaptiveColor::rgb(0xe6e6e9, 0xe6e6e9).opacity(0.7)),
        )
        .child(
            view()
                .flex_row()
                .gap(px(10.0))
                .child(hour_cycle_toggle(twelve_hour))
                .child(night_dim_toggle(night_dim)),
        )
}

fn hour_cycle_toggle(twelve_hour: Signal<bool>) -> impl IntoElement {
    view()
        .role(AccessibilityRole::Button)
        .test_id("hour_cycle_toggle")
        .label("Switch clock format")
        .hint(move || {
            if twelve_hour.get() {
                "Switches to 24-hour time"
            } else {
                "Switches to 12-hour time"
            }
        })
        .on_press(move || twelve_hour.update(|v| *v = !*v))
        .min_w(px(44.0))
        .min_h(px(44.0))
        .px(px(14.0))
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .border_color(rgba(0xffffff22))
        .bg(rgba(0xffffff0f))
        .active(|s| s.bg(rgba(0xffffff26)))
        .child(
            text(move || if twelve_hour.get() { "12h" } else { "24h" })
                .text_size(px(13.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(white()),
        )
}

fn night_dim_toggle(night_dim: Signal<bool>) -> impl IntoElement {
    view()
        .role(AccessibilityRole::Switch)
        .test_id("night_dim_toggle")
        .label("Night dim")
        .accessibility_state(move || AccessibilityState {
            checked: Some(if night_dim.get() { Checked::True } else { Checked::False }),
            ..AccessibilityState::default()
        })
        .on_press(move || night_dim.update(|v| *v = !*v))
        .min_w(px(44.0))
        .min_h(px(44.0))
        .px(px(14.0))
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .border_color(rgba(0xffffff22))
        .bg(move || if night_dim.get() { rgba(0xffd27855) } else { rgba(0xffffff0f) })
        .child(
            text("Dim")
                .text_size(px(13.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(white()),
        )
}

fn clock_block(
    clock_display: Memo<(String, &'static str)>,
    date_display: Memo<String>,
) -> impl IntoElement {
    view()
        .gap(px(6.0))
        .child(
            text("")
                .test_id("clock_time")
                .text_size(px(72.0))
                .font_weight(FontWeight::BOLD)
                .tabular_nums()
                .text_color(white())
                .child(text(move || clock_display.get().0))
                .child(
                    text(move || clock_display.get().1)
                        .text_size(px(24.0))
                        .text_color(rgba(0xffffffaa)),
                ),
        )
        .child(
            text(move || date_display.get())
                .test_id("clock_date")
                .text_size(px(18.0))
                .text_color(rgba(0xffffffaa)),
        )
}

fn focus_timer_block(
    preset_index: Signal<usize>,
    remaining_secs: Signal<i64>,
    running: Signal<bool>,
) -> impl IntoElement {
    view()
        .gap(px(16.0))
        .p(px(20.0))
        .rounded(px(16.0))
        .border_1()
        .border_color(rgba(0xffffff1a))
        .bg(rgba(0xffffff08))
        .child(
            text("Focus timer")
                .role(AccessibilityRole::Header)
                .text_size(px(15.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgba(0xffffffaa)),
        )
        .child(
            text(move || format_remaining(remaining_secs.get()))
                .test_id("timer_remaining")
                .role(AccessibilityRole::Timer)
                .text_size(px(48.0))
                .font_weight(FontWeight::BOLD)
                .tabular_nums()
                .text_color(white()),
        )
        .child(
            view()
                .flex_row()
                .flex_wrap()
                .gap(px(8.0))
                .child((0..PRESETS_MIN.len()).map(|i| preset_button(i, preset_index, remaining_secs, running))),
        )
        .child(
            view()
                .flex_row()
                .gap(px(10.0))
                .child(run_pause_button(remaining_secs, running))
                .child(reset_button(preset_index, remaining_secs, running)),
        )
}

fn preset_button(
    i: usize,
    preset_index: Signal<usize>,
    remaining_secs: Signal<i64>,
    running: Signal<bool>,
) -> impl IntoElement {
    let minutes = PRESETS_MIN[i];
    let test_id = PRESET_TEST_IDS[i];
    let label = PRESET_LABELS[i];
    let text_str = PRESET_TEXTS[i];
    view()
        .role(AccessibilityRole::Button)
        .test_id(test_id)
        .label(label)
        .min_w(px(44.0))
        .min_h(px(44.0))
        .px(px(14.0))
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .border_color(rgba(0xffffff22))
        .bg(move || if preset_index.get() == i { rgba(0xffffff33) } else { rgba(0xffffff0a) })
        .on_press(move || {
            preset_index.set(i);
            remaining_secs.set(minutes * 60);
            running.set(false);
        })
        .child(
            text(text_str)
                .text_size(px(14.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(white()),
        )
}

fn run_pause_button(remaining_secs: Signal<i64>, running: Signal<bool>) -> impl IntoElement {
    view()
        .role(AccessibilityRole::Button)
        .test_id("timer_toggle")
        .label(move || if running.get() { "Pause" } else { "Start" })
        .min_h(px(44.0))
        .flex_1()
        .items_center()
        .justify_center()
        .rounded(px(12.0))
        .bg(move || {
            if running.get() {
                AdaptiveColor::from(rgba(0xffd27833))
            } else {
                theme().primary
            }
        })
        .active(|s| s.opacity(0.75))
        .on_press(move || {
            if remaining_secs.get_untracked() <= 0 {
                return;
            }
            running.update(|r| *r = !*r);
        })
        .child(
            text(move || if running.get() { "Pause" } else { "Start" })
                .text_size(px(15.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(move || {
                    if running.get() {
                        AdaptiveColor::from(white())
                    } else {
                        theme().primary_foreground
                    }
                }),
        )
}

fn reset_button(
    preset_index: Signal<usize>,
    remaining_secs: Signal<i64>,
    running: Signal<bool>,
) -> impl IntoElement {
    view()
        .role(AccessibilityRole::Button)
        .test_id("timer_reset")
        .label("Reset")
        .min_w(px(44.0))
        .min_h(px(44.0))
        .px(px(16.0))
        .items_center()
        .justify_center()
        .rounded(px(12.0))
        .border_1()
        .border_color(rgba(0xffffff22))
        .active(|s| s.bg(rgba(0xffffff1a)))
        .on_press(move || {
            running.set(false);
            remaining_secs.set(PRESETS_MIN[preset_index.get()] * 60);
        })
        .child(
            text("Reset")
                .text_size(px(15.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(white()),
        )
}

/// Music and camera, shown as what they actually are: not connected, not configured. No
/// metadata, no transport controls — there is nothing behind them to control yet. See
/// docs/spotify-feasibility.md.
fn status_row() -> impl IntoElement {
    view()
        .flex_row()
        .flex_wrap()
        .gap(px(12.0))
        .child(status_card(
            "music_status",
            "Music",
            "Not connected",
            "Spotify playback stays on the Windows PC. No remote-control path is proven yet, so no controls are shown.",
        ))
        .child(status_card(
            "camera_status",
            "Camera",
            "Not configured",
            "Imou setup has not been added. No stream or deep link is wired up.",
        ))
}

fn status_card(
    test_id: &'static str,
    title: &'static str,
    state: &'static str,
    detail: &'static str,
) -> impl IntoElement {
    view()
        .accessible(true)
        .test_id(test_id)
        .flex_1()
        .min_w(px(220.0))
        .gap(px(6.0))
        .p(px(16.0))
        .rounded(px(14.0))
        .border_1()
        .border_color(rgba(0xffffff1a))
        .bg(rgba(0xffffff06))
        .child(
            text(title)
                .text_size(px(13.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgba(0xffffff88)),
        )
        .child(
            text(state)
                .text_size(px(17.0))
                .font_weight(FontWeight::BOLD)
                .text_color(rgba(0xffd278ff)),
        )
        .child(
            text(detail)
                .text_size(px(12.0))
                .line_height(px(17.0))
                .text_color(rgba(0xffffff66)),
        )
}

/// A click-through dark overlay for night use; fades in/out rather than snapping.
fn night_dim_overlay(night_dim: Signal<bool>) -> impl IntoElement {
    view()
        .absolute()
        .inset_0()
        .pointer_events(PointerEvents::None)
        .bg(black())
        .opacity(move || if night_dim.get() { 0.45_f32 } else { 0.0_f32 })
        .transition_opacity()
        .duration(250.0_f32)
        // Decorative; never intercepts touches and is not announced on its own.
        .accessibility_elements_hidden(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nsh_engine::{EngineConfig, Root};
    use nsh_testing::Headless;

    #[test]
    fn clock_formats_12_and_24_hour() {
        assert_eq!(format_clock(13, 5, 9, false), ("13:05:09".to_string(), ""));
        assert_eq!(format_clock(13, 5, 9, true), ("01:05:09".to_string(), "PM"));
    }

    #[test]
    fn midnight_is_12_am_in_12_hour_mode() {
        assert_eq!(format_clock(0, 0, 0, true), ("12:00:00".to_string(), "AM"));
    }

    #[test]
    fn date_matches_known_day() {
        assert_eq!(
            format_date_parts("Thursday", "January", 1, 1970),
            "Thursday, January 1, 1970"
        );
    }

    #[test]
    fn remaining_time_formats_as_mm_ss() {
        assert_eq!(format_remaining(65), "01:05");
        assert_eq!(format_remaining(0), "00:00");
        assert_eq!(format_remaining(-5), "00:00");
    }

    #[test]
    fn dashboard_shows_clock_and_honest_status() {
        let mut app = Headless::start(EngineConfig::new(Root::of::<Dashboard>())).unwrap();
        app.wait_for("id:clock_time").unwrap();
        app.wait_for("Not connected").unwrap();
        app.wait_for("Not configured").unwrap();
    }

    #[test]
    fn focus_timer_starts_and_resets() {
        let mut app = Headless::start(EngineConfig::new(Root::of::<Dashboard>())).unwrap();
        app.wait_for("id:timer_remaining").unwrap();
        app.press("id:timer_toggle").unwrap();
        app.press("id:timer_reset").unwrap();
        app.wait_for("15:00").unwrap();
    }
}
