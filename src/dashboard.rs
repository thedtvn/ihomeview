//! The ihomeview dashboard root.
//!
//! A desk display for a landscape iPad (1024x768 first, portrait still works):
//!
//! * a big clock and date, with 12h/24h, Dim and a Timer button;
//! * a Music card that fills the screen and follows [`crate::spotify::SpotifyState`];
//! * a compact Camera tile ("Not configured");
//! * a focus timer that is hidden until it is needed: a header button opens it, a running
//!   timer stays visible as a small countdown in the header, and finishing or resetting hides it.
//!
//! No continuous animation and no blur; the only repeating work is the clock tick and, while a
//! timer runs, its 250 ms deadline check. Pure NSH Rust UI, no WebView.

use std::time::Duration;

use chrono::{Datelike, Local, Timelike};
use nsh_base::theme;
use nsh_core::{Checked, prelude::*, time::unix_ms};

use crate::spotify::{self, Playback, SpotifyCommand, SpotifyState};

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
const DEFAULT_PRESET: usize = 1; // 15 minutes.

// Palette: near-black desk display, warm amber for the timer and Dim, green for Music.
const MUTED: u32 = 0xffffff99;
const FAINT: u32 = 0xffffff66;
const HAIRLINE: u32 = 0xffffff1a;
const CARD: u32 = 0xffffff0d;
const AMBER: u32 = 0xffd278ff;
const GREEN: u32 = 0x1ed760ff;
const ON_GREEN: u32 = 0x07130bff;

fn is_landscape() -> bool {
    window().width > window().height
}

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

/// Progress through a track, 0.0..=1.0.
fn progress_fraction(progress_ms: u64, duration_ms: u64) -> f32 {
    if duration_ms == 0 {
        return 0.0;
    }
    (progress_ms as f32 / duration_ms as f32).clamp(0.0, 1.0)
}

/// `mm:ss` for a track position given in milliseconds.
fn format_track_time(ms: u64) -> String {
    format_remaining((ms / 1000) as i64)
}

/// Which Music screen to show. Used so the card is only rebuilt when the screen changes,
/// not on every progress update.
fn spotify_kind(state: &SpotifyState) -> u8 {
    match state {
        SpotifyState::Unavailable(_) => 0,
        SpotifyState::SignedOut => 1,
        SpotifyState::AwaitingApproval { .. } => 2,
        SpotifyState::Connecting => 3,
        SpotifyState::Connected(_) => 4,
        SpotifyState::Error(_) => 5,
    }
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

    // Spotify: the backend owns the state, the UI only reads it and sends commands.
    let spotify_state = signal(SpotifyState::SignedOut);
    spotify::start(spotify_state);

    // Focus timer state. Hidden until `timer_open`; see `timer_header_button`.
    let preset_index = signal(DEFAULT_PRESET);
    let remaining_secs = signal(PRESETS_MIN[DEFAULT_PRESET] * 60);
    let running = signal(false);
    let timer_open = signal(false);
    let timer_finished = signal(false);

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
                    // Finished: put the timer away and flag it in the header until tapped.
                    remaining_secs.set(PRESETS_MIN[preset_index.get_untracked()] * 60);
                    running.set(false);
                    timer_open.set(false);
                    timer_finished.set(true);
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
            scroll().flex_1().child(
                view()
                    .p(px(24.0))
                    .gap(px(20.0))
                    // Landscape: exactly one screen tall so the Music card takes what is left.
                    // Portrait: natural height, the scroll view handles the rest.
                    .style_when(is_landscape, |s| s.h(window().height))
                    .child(top_band(
                        clock_display,
                        date_display,
                        twelve_hour,
                        night_dim,
                        preset_index,
                        remaining_secs,
                        running,
                        timer_open,
                        timer_finished,
                    ))
                    .child(
                        view()
                            .gap(px(20.0))
                            .style_when(is_landscape, |s| s.flex_1().flex_row())
                            .child(music_card(spotify_state))
                            .child(when(timer_open, move || {
                                focus_timer_panel(preset_index, remaining_secs, running, timer_open)
                            })),
                    ),
            ),
        )
        .child(night_dim_overlay(night_dim))
}

// ---------------------------------------------------------------------------------------------
// Top band: clock on the left, controls and camera on the right.
// ---------------------------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn top_band(
    clock_display: Memo<(String, &'static str)>,
    date_display: Memo<String>,
    twelve_hour: Signal<bool>,
    night_dim: Signal<bool>,
    preset_index: Signal<usize>,
    remaining_secs: Signal<i64>,
    running: Signal<bool>,
    timer_open: Signal<bool>,
    timer_finished: Signal<bool>,
) -> impl IntoElement {
    view()
        .gap(px(16.0))
        .style_when(is_landscape, |s| s.flex_row().justify_between().items_start())
        .child(clock_block(clock_display, date_display))
        .child(
            view()
                .gap(px(12.0))
                .style_when(is_landscape, |s| s.w(px(320.0)))
                .child(
                    view()
                        .flex_row()
                        .flex_wrap()
                        .gap(px(10.0))
                        .style_when(is_landscape, |s| s.justify_end())
                        .child(timer_header_button(
                            preset_index,
                            remaining_secs,
                            running,
                            timer_open,
                            timer_finished,
                        ))
                        .child(hour_cycle_toggle(twelve_hour))
                        .child(night_dim_toggle(night_dim)),
                )
                .child(camera_tile()),
        )
}

fn clock_block(
    clock_display: Memo<(String, &'static str)>,
    date_display: Memo<String>,
) -> impl IntoElement {
    view()
        .gap(px(2.0))
        .child(
            text("")
                .test_id("clock_time")
                .text_size(px(104.0))
                .font_weight(FontWeight::BOLD)
                .tabular_nums()
                .text_color(white())
                .child(text(move || clock_display.get().0))
                .child(
                    text(move || clock_display.get().1)
                        .text_size(px(30.0))
                        .text_color(rgba(MUTED)),
                ),
        )
        .child(
            text(move || date_display.get())
                .test_id("clock_date")
                .text_size(px(26.0))
                .text_color(rgba(MUTED)),
        )
}

fn header_chip_text(content: &'static str) -> Text {
    text(content)
        .text_size(px(16.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(white())
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
        .min_w(px(56.0))
        .min_h(px(48.0))
        .px(px(16.0))
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .border_color(rgba(0xffffff22))
        .bg(rgba(0xffffff0f))
        .active(|s| s.bg(rgba(0xffffff26)))
        .child(
            text(move || if twelve_hour.get() { "12h" } else { "24h" })
                .text_size(px(16.0))
                .font_weight(FontWeight::SEMIBOLD)
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
        .min_w(px(56.0))
        .min_h(px(48.0))
        .px(px(16.0))
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .border_color(rgba(0xffffff22))
        .bg(move || if night_dim.get() { rgba(0xffd27855) } else { rgba(0xffffff0f) })
        .child(header_chip_text("Dim"))
}

/// Compact camera tile. Imou is not set up, so it only says so.
fn camera_tile() -> impl IntoElement {
    view()
        .accessible(true)
        .label("Camera, not configured")
        .test_id("camera_status")
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(12.0))
        .px(px(18.0))
        .min_h(px(56.0))
        .rounded(px(16.0))
        .border_1()
        .border_color(rgba(HAIRLINE))
        .bg(rgba(CARD))
        .child(
            text("Camera")
                .text_size(px(16.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgba(MUTED)),
        )
        .child(
            text("Not configured")
                .text_size(px(16.0))
                .font_weight(FontWeight::BOLD)
                .text_color(rgba(AMBER)),
        )
}

// ---------------------------------------------------------------------------------------------
// Focus timer: header button + panel, only shown when needed.
// ---------------------------------------------------------------------------------------------

/// The one header control for the timer:
/// idle → "Timer" (opens the panel); running or paused → the remaining time (toggles the panel);
/// just finished → "Time's up" until tapped.
fn timer_header_button(
    preset_index: Signal<usize>,
    remaining_secs: Signal<i64>,
    running: Signal<bool>,
    timer_open: Signal<bool>,
    timer_finished: Signal<bool>,
) -> impl IntoElement {
    // 0 idle, 1 engaged (running or paused part-way), 2 finished.
    let mode = computed(move || {
        if timer_finished.get() {
            2u8
        } else if running.get() || remaining_secs.get() < PRESETS_MIN[preset_index.get()] * 60 {
            1
        } else {
            0
        }
    });
    dynamic(move || match mode.get() {
        2 => view()
            .role(AccessibilityRole::Button)
            .test_id("timer_finished")
            .label("Focus timer finished")
            .hint("Dismisses the alert")
            .on_press(move || timer_finished.set(false))
            .min_h(px(48.0))
            .px(px(20.0))
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(rgba(AMBER))
            .active(|s| s.opacity(0.8))
            .child(
                text("Time's up")
                    .text_size(px(16.0))
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgba(0x1a1203ff)),
            )
            .into_element(),
        1 => view()
            .role(AccessibilityRole::Button)
            .test_id("timer_indicator")
            .label(move || {
                format!(
                    "Focus timer, {} left{}",
                    format_remaining(remaining_secs.get()),
                    if running.get() { "" } else { ", paused" }
                )
            })
            .hint("Shows or hides the timer")
            .on_press(move || timer_open.update(|v| *v = !*v))
            .min_h(px(48.0))
            .px(px(18.0))
            .items_center()
            .justify_center()
            .rounded_full()
            .border_1()
            .border_color(rgba(0xffd27888))
            .bg(rgba(0xffd27833))
            .active(|s| s.opacity(0.8))
            .child(
                text(move || {
                    let t = format_remaining(remaining_secs.get());
                    if running.get() { t } else { format!("{t} paused") }
                })
                .role(AccessibilityRole::Timer)
                .text_size(px(18.0))
                .font_weight(FontWeight::BOLD)
                .tabular_nums()
                .text_color(rgba(AMBER)),
            )
            .into_element(),
        _ => view()
            .role(AccessibilityRole::Button)
            .test_id("timer_open")
            .label("Focus timer")
            .hint("Opens the timer")
            .on_press(move || timer_open.set(true))
            .min_w(px(56.0))
            .min_h(px(48.0))
            .px(px(18.0))
            .items_center()
            .justify_center()
            .rounded_full()
            .border_1()
            .border_color(rgba(0xffffff22))
            .bg(rgba(0xffffff0f))
            .active(|s| s.bg(rgba(0xffffff26)))
            .child(header_chip_text("Timer"))
            .into_element(),
    })
}

fn focus_timer_panel(
    preset_index: Signal<usize>,
    remaining_secs: Signal<i64>,
    running: Signal<bool>,
    timer_open: Signal<bool>,
) -> impl IntoElement {
    view()
        .test_id("timer_panel")
        .gap(px(16.0))
        .p(px(20.0))
        .rounded(px(24.0))
        .border_1()
        .border_color(rgba(HAIRLINE))
        .bg(rgba(CARD))
        .style_when(is_landscape, |s| s.w(px(300.0)))
        .child(
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .child(
                    text("Focus timer")
                        .role(AccessibilityRole::Header)
                        .text_size(px(18.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgba(MUTED)),
                )
                .child(
                    view()
                        .role(AccessibilityRole::Button)
                        .test_id("timer_close")
                        .label("Close timer")
                        .hint("Hides the timer; a running countdown keeps going")
                        .on_press(move || timer_open.set(false))
                        .min_w(px(48.0))
                        .min_h(px(48.0))
                        .px(px(14.0))
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(rgba(0xffffff12))
                        .active(|s| s.bg(rgba(0xffffff26)))
                        .child(
                            text("Close")
                                .text_size(px(15.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(white()),
                        ),
                ),
        )
        .child(
            text(move || format_remaining(remaining_secs.get()))
                .test_id("timer_remaining")
                .role(AccessibilityRole::Timer)
                .text_size(px(72.0))
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
                .child(reset_button(preset_index, remaining_secs, running, timer_open)),
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
        .min_w(px(56.0))
        .min_h(px(48.0))
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
                .text_size(px(16.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(white()),
        )
}

fn run_pause_button(remaining_secs: Signal<i64>, running: Signal<bool>) -> impl IntoElement {
    view()
        .role(AccessibilityRole::Button)
        .test_id("timer_toggle")
        .label(move || if running.get() { "Pause" } else { "Start" })
        .min_h(px(56.0))
        .flex_1()
        .items_center()
        .justify_center()
        .rounded(px(14.0))
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
                .text_size(px(18.0))
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

/// Reset stops the countdown, restores the preset time and puts the timer away.
fn reset_button(
    preset_index: Signal<usize>,
    remaining_secs: Signal<i64>,
    running: Signal<bool>,
    timer_open: Signal<bool>,
) -> impl IntoElement {
    view()
        .role(AccessibilityRole::Button)
        .test_id("timer_reset")
        .label("Reset")
        .hint("Resets the timer and hides it")
        .min_w(px(72.0))
        .min_h(px(56.0))
        .px(px(16.0))
        .items_center()
        .justify_center()
        .rounded(px(14.0))
        .border_1()
        .border_color(rgba(0xffffff22))
        .active(|s| s.bg(rgba(0xffffff1a)))
        .on_press(move || {
            running.set(false);
            remaining_secs.set(PRESETS_MIN[preset_index.get()] * 60);
            timer_open.set(false);
        })
        .child(
            text("Reset")
                .text_size(px(18.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(white()),
        )
}

// ---------------------------------------------------------------------------------------------
// Music card: the main content, driven by the Spotify state.
// ---------------------------------------------------------------------------------------------

/// Big rounded button used across the Music states. `primary` = green fill.
fn music_button(
    spotify_state: Signal<SpotifyState>,
    command: SpotifyCommand,
    test_id: &'static str,
    label: &'static str,
    primary: bool,
) -> impl IntoElement {
    view()
        .role(AccessibilityRole::Button)
        .test_id(test_id)
        .label(label)
        .on_press(move || spotify::send(spotify_state, command))
        .min_h(px(60.0))
        .min_w(px(120.0))
        .px(px(28.0))
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .border_color(if primary { rgba(GREEN) } else { rgba(0xffffff33) })
        .bg(if primary { rgba(GREEN) } else { rgba(0xffffff0f) })
        .active(|s| s.opacity(0.75))
        .child(
            text(label)
                .text_size(px(22.0))
                .font_weight(FontWeight::BOLD)
                .text_color(if primary { rgba(ON_GREEN) } else { rgba(0xffffffff) }),
        )
}

fn state_title(content: &'static str) -> Text {
    text(content)
        .role(AccessibilityRole::Header)
        .text_size(px(40.0))
        .font_weight(FontWeight::BOLD)
        .text_color(white())
}

fn state_note(content: impl IntoReactive<SharedString>) -> Text {
    text(content)
        .text_size(px(22.0))
        .line_height(px(30.0))
        .text_color(rgba(MUTED))
}

/// A centered column for the message-style states.
fn state_column(test_id: &'static str) -> View {
    view()
        .test_id(test_id)
        .flex_1()
        .items_center()
        .justify_center()
        .gap(px(18.0))
        .py(px(24.0))
}

fn music_card(spotify_state: Signal<SpotifyState>) -> impl IntoElement {
    let kind = computed(move || spotify_kind(&spotify_state.get()));
    let playback = computed(move || match spotify_state.get() {
        SpotifyState::Connected(p) => p,
        _ => Playback::default(),
    });
    let message = computed(move || match spotify_state.get() {
        SpotifyState::Unavailable(m) | SpotifyState::Error(m) => m,
        _ => String::new(),
    });
    let approval = computed(move || match spotify_state.get() {
        SpotifyState::AwaitingApproval { code, url } => (code, url),
        _ => (String::new(), String::new()),
    });

    view()
        .test_id("music_status")
        .gap(px(12.0))
        .p(px(24.0))
        .rounded(px(28.0))
        .border_1()
        .border_color(rgba(HAIRLINE))
        .bg(rgba(CARD))
        .style_when(is_landscape, |s| s.flex_1())
        .child(
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .min_h(px(48.0))
                .child(
                    text("Music")
                        .role(AccessibilityRole::Header)
                        .text_size(px(18.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgba(GREEN)),
                )
                .child(when(move || kind.get() == 4, move || sign_out_button(spotify_state))),
        )
        .child(
            view()
                .flex_1()
                .child(dynamic(move || match kind.get() {
                    0 => unavailable_view(message).into_element(),
                    1 => signed_out_view(spotify_state).into_element(),
                    2 => awaiting_view(spotify_state, approval).into_element(),
                    3 => connecting_view().into_element(),
                    4 => connected_view(spotify_state, playback).into_element(),
                    _ => error_view(spotify_state, message).into_element(),
                })),
        )
}

fn sign_out_button(spotify_state: Signal<SpotifyState>) -> impl IntoElement {
    view()
        .role(AccessibilityRole::Button)
        .test_id("spotify_sign_out")
        .label("Sign out of Spotify")
        .on_press(move || spotify::send(spotify_state, SpotifyCommand::SignOut))
        .min_h(px(48.0))
        .px(px(20.0))
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .border_color(rgba(0xffffff33))
        .active(|s| s.bg(rgba(0xffffff1a)))
        .child(
            text("Sign out")
                .text_size(px(16.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(white()),
        )
}

fn unavailable_view(reason: Memo<String>) -> impl IntoElement {
    state_column("spotify_unavailable")
        .child(state_title("Spotify unavailable"))
        .child(state_note(move || reason.get()).text_center().max_w(px(560.0)))
}

fn signed_out_view(spotify_state: Signal<SpotifyState>) -> impl IntoElement {
    state_column("spotify_signed_out")
        .child(state_title("Control Spotify here"))
        .child(state_note("Sign in once to play, pause and skip."))
        .child(music_button(
            spotify_state,
            SpotifyCommand::SignIn,
            "spotify_sign_in",
            "Sign in to Spotify",
            true,
        ))
}

fn awaiting_view(
    spotify_state: Signal<SpotifyState>,
    approval: Memo<(String, String)>,
) -> impl IntoElement {
    state_column("spotify_awaiting")
        .child(state_note("On your phone or computer, open"))
        .child(
            text(move || approval.get().1)
                .test_id("spotify_url")
                .text_size(px(30.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(white())
                .text_center(),
        )
        .child(state_note("and enter this code"))
        .child(
            text(move || approval.get().0)
                .test_id("spotify_code")
                .text_size(px(104.0))
                .font_weight(FontWeight::BOLD)
                .tabular_nums()
                .letter_spacing(px(10.0))
                .text_color(rgba(GREEN)),
        )
        .child(music_button(
            spotify_state,
            SpotifyCommand::SignOut,
            "spotify_cancel",
            "Cancel",
            false,
        ))
}

fn connecting_view() -> impl IntoElement {
    state_column("spotify_connecting").child(
        text("Connecting to Spotify…")
            .text_size(px(28.0))
            .font_weight(FontWeight::MEDIUM)
            .text_color(rgba(FAINT)),
    )
}

fn error_view(spotify_state: Signal<SpotifyState>, message: Memo<String>) -> impl IntoElement {
    state_column("spotify_error")
        .child(state_title("Spotify problem"))
        .child(state_note(move || message.get()).text_center().max_w(px(560.0)))
        .child(music_button(
            spotify_state,
            SpotifyCommand::SignIn,
            "spotify_retry",
            "Retry",
            true,
        ))
}

fn connected_view(spotify_state: Signal<SpotifyState>, playback: Memo<Playback>) -> impl IntoElement {
    let has_track = computed(move || playback.get().title.is_some());
    let artwork = computed(move || playback.get().artwork_url);
    let device_line = move || match playback.get().device {
        Some(d) => format!("Playing on {d}"),
        None => "No active device".to_string(),
    };

    view()
        .test_id("spotify_connected")
        .flex_1()
        .child(
            when(move || has_track.get(), move || {
                view()
                    .test_id("spotify_track")
                    .flex_1()
                    .gap(px(28.0))
                    .items_center()
                    .justify_center()
                    .style_when(is_landscape, |s| s.flex_row())
                    .child(album_art(artwork))
                    .child(
                        view()
                            .gap(px(10.0))
                            .style_when(is_landscape, |s| s.flex_1())
                            .child(
                                text(device_line)
                                    .test_id("spotify_device")
                                    .text_size(px(18.0))
                                    .text_color(rgba(GREEN)),
                            )
                            .child(
                                text(move || playback.get().title.unwrap_or_default())
                                    .test_id("spotify_title")
                                    .text_size(px(44.0))
                                    .line_height(px(50.0))
                                    .font_weight(FontWeight::BOLD)
                                    .number_of_lines(2)
                                    .text_color(white()),
                            )
                            .child(
                                text(move || playback.get().artists.unwrap_or_default())
                                    .test_id("spotify_artists")
                                    .text_size(px(26.0))
                                    .number_of_lines(1)
                                    .text_color(rgba(0xffffffd0)),
                            )
                            .child(
                                text(move || playback.get().album.unwrap_or_default())
                                    .test_id("spotify_album")
                                    .text_size(px(20.0))
                                    .number_of_lines(1)
                                    .text_color(rgba(FAINT)),
                            )
                            .child(progress_block(playback))
                            .child(transport_row(spotify_state, playback)),
                    )
            })
            .otherwise(move || {
                state_column("spotify_idle")
                    .child(state_title("Nothing playing"))
                    .child(state_note(move || match playback.get().device {
                        Some(d) => format!("{d} is ready. Start a song in Spotify."),
                        None => "Start a song in Spotify on any device.".to_string(),
                    }))
                    .child(transport_row(spotify_state, playback))
            }),
        )
}

fn album_art(artwork: Memo<Option<String>>) -> impl IntoElement {
    let side = || if is_landscape() { px(380.0) } else { px(300.0) };
    when(move || artwork.get().is_some(), move || {
        image(move || artwork.get().unwrap_or_default())
            .test_id("spotify_art")
            .label("Album art")
            .w(side)
            .h(side)
            .rounded(px(24.0))
            .bg(rgba(0xffffff0f))
    })
    .otherwise(move || {
        view()
            .test_id("spotify_art_placeholder")
            .w(side)
            .h(side)
            .rounded(px(24.0))
            .bg(rgba(0xffffff0f))
            .border_1()
            .border_color(rgba(HAIRLINE))
            .items_center()
            .justify_center()
            .child(
                text("No artwork")
                    .text_size(px(18.0))
                    .text_color(rgba(FAINT)),
            )
    })
}

fn progress_block(playback: Memo<Playback>) -> impl IntoElement {
    view()
        .gap(px(8.0))
        .py(px(8.0))
        .child(
            view()
                .test_id("spotify_progress")
                .h(px(8.0))
                .rounded_full()
                .overflow_hidden()
                .bg(rgba(0xffffff22))
                .child(
                    view()
                        .h(px(8.0))
                        .rounded_full()
                        .bg(rgba(GREEN))
                        .w(move || {
                            let p = playback.get();
                            relative(progress_fraction(p.progress_ms, p.duration_ms))
                        }),
                ),
        )
        .child(
            view()
                .flex_row()
                .justify_between()
                .child(
                    text(move || format_track_time(playback.get().progress_ms))
                        .text_size(px(16.0))
                        .tabular_nums()
                        .text_color(rgba(MUTED)),
                )
                .child(
                    text(move || format_track_time(playback.get().duration_ms))
                        .text_size(px(16.0))
                        .tabular_nums()
                        .text_color(rgba(MUTED)),
                ),
        )
}

fn transport_row(spotify_state: Signal<SpotifyState>, playback: Memo<Playback>) -> impl IntoElement {
    let can_control = computed(move || playback.get().can_control);
    view()
        .flex_row()
        .items_center()
        .gap(px(16.0))
        .pt(px(8.0))
        .child(transport_button(
            spotify_state,
            can_control,
            SpotifyCommand::Previous,
            "spotify_prev",
            || "Previous",
            false,
        ))
        .child(transport_button(
            spotify_state,
            can_control,
            SpotifyCommand::PlayPause,
            "spotify_play_pause",
            move || if playback.get().is_playing { "Pause" } else { "Play" },
            true,
        ))
        .child(transport_button(
            spotify_state,
            can_control,
            SpotifyCommand::Next,
            "spotify_next",
            || "Next",
            false,
        ))
}

fn transport_button(
    spotify_state: Signal<SpotifyState>,
    can_control: Memo<bool>,
    command: SpotifyCommand,
    test_id: &'static str,
    label: impl Fn() -> &'static str + Clone + 'static,
    primary: bool,
) -> impl IntoElement {
    let label_for_text = label.clone();
    view()
        .role(AccessibilityRole::Button)
        .test_id(test_id)
        .label(move || label())
        .disabled(move || !can_control.get())
        .opacity(move || if can_control.get() { 1.0_f32 } else { 0.4_f32 })
        .on_press(move || spotify::send(spotify_state, command))
        .min_h(px(72.0))
        .min_w(px(if primary { 140.0 } else { 110.0 }))
        .px(px(24.0))
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .border_color(if primary { rgba(GREEN) } else { rgba(0xffffff33) })
        .bg(if primary { rgba(GREEN) } else { rgba(0xffffff0f) })
        .active(|s| s.opacity(0.75))
        .child(
            text(move || label_for_text())
                .text_size(px(24.0))
                .font_weight(FontWeight::BOLD)
                .text_color(if primary { rgba(ON_GREEN) } else { rgba(0xffffffff) }),
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
    use nsh_core::geometry::{Points, Size};
    use nsh_engine::{EngineConfig, Root};
    use nsh_testing::Headless;
    use std::time::Duration;

    fn landscape_config(root: Root) -> EngineConfig {
        let mut config = EngineConfig::new(root);
        config.viewport = Size::new(Points(1024.0), Points(768.0));
        config.environment.window = config.viewport;
        config
    }

    fn start_landscape() -> Headless {
        Headless::start(landscape_config(Root::of::<Dashboard>())).unwrap()
    }

    // Probes render the Music card alone in a given state.
    fn probe(state: SpotifyState) -> impl IntoElement {
        let s = signal(state);
        view().flex_1().p(px(24.0)).child(music_card(s))
    }

    #[component]
    fn ProbeSignedOut() -> impl IntoElement {
        probe(SpotifyState::SignedOut)
    }

    #[component]
    fn ProbeAwaiting() -> impl IntoElement {
        probe(SpotifyState::AwaitingApproval {
            code: "ABCD-1234".into(),
            url: "spotify.com/pair".into(),
        })
    }

    #[component]
    fn ProbeConnecting() -> impl IntoElement {
        probe(SpotifyState::Connecting)
    }

    #[component]
    fn ProbeError() -> impl IntoElement {
        probe(SpotifyState::Error("Login expired.".into()))
    }

    #[component]
    fn ProbeConnectedLocked() -> impl IntoElement {
        probe(SpotifyState::Connected(Playback {
            device: Some("Windows PC".into()),
            title: Some("Test Song".into()),
            artists: Some("Test Artist".into()),
            album: Some("Test Album".into()),
            artwork_url: None,
            is_playing: true,
            progress_ms: 61_000,
            duration_ms: 180_000,
            can_control: false,
        }))
    }

    #[component]
    fn ProbeConnectedControllable() -> impl IntoElement {
        probe(SpotifyState::Connected(Playback {
            device: Some("Windows PC".into()),
            title: Some("Test Song".into()),
            can_control: true,
            ..Playback::default()
        }))
    }

    #[component]
    fn ProbeNothingPlaying() -> impl IntoElement {
        probe(SpotifyState::Connected(Playback::default()))
    }

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
    fn progress_fraction_is_clamped() {
        assert_eq!(progress_fraction(0, 0), 0.0);
        assert_eq!(progress_fraction(50, 100), 0.5);
        assert_eq!(progress_fraction(500, 100), 1.0);
        assert_eq!(format_track_time(61_000), "01:01");
    }

    #[test]
    fn dashboard_shows_clock_music_sign_in_and_camera() {
        let mut app = start_landscape();
        app.wait_for("id:clock_time").unwrap();
        app.wait_for("id:camera_status").unwrap();
        app.wait_for("Not configured").unwrap();
        app.wait_for("id:music_status").unwrap();
        // No saved Spotify login in a fresh app: offer sign-in, no playback controls.
        app.wait_for("id:spotify_sign_in").unwrap();
        assert_eq!(app.screen().count("id:spotify_play_pause").unwrap(), 0);
        assert_eq!(app.screen().count("id:spotify_unavailable").unwrap(), 0);
    }

    #[test]
    fn timer_is_hidden_until_opened_then_shows_running_indicator() {
        let mut app = start_landscape();
        app.wait_for("id:timer_open").unwrap();
        // Hidden by default: no panel, no countdown, no indicator.
        assert_eq!(app.screen().count("id:timer_panel").unwrap(), 0);
        assert_eq!(app.screen().count("id:timer_remaining").unwrap(), 0);
        assert_eq!(app.screen().count("id:timer_indicator").unwrap(), 0);

        app.press("id:timer_open").unwrap();
        app.wait_for("id:timer_remaining").unwrap();
        app.wait_for("15:00").unwrap();

        app.press("id:timer_toggle").unwrap();
        app.wait_for("id:timer_indicator").unwrap();
        assert_eq!(app.screen().count("id:timer_open").unwrap(), 0);

        // Closing the panel keeps the running indicator.
        app.press("id:timer_close").unwrap();
        app.wait_for_gone("id:timer_panel", Duration::from_secs(5)).unwrap();
        app.wait_for("id:timer_indicator").unwrap();

        // Tapping the indicator expands it again.
        app.press("id:timer_indicator").unwrap();
        app.wait_for("id:timer_panel").unwrap();
    }

    #[test]
    fn timer_reset_restores_time_and_hides_it() {
        let mut app = start_landscape();
        app.wait_for("id:timer_open").unwrap();
        app.press("id:timer_open").unwrap();
        app.wait_for("id:timer_preset_5").unwrap();
        app.press("id:timer_preset_5").unwrap();
        app.wait_for("05:00").unwrap();
        app.press("id:timer_toggle").unwrap();
        app.wait_for("id:timer_indicator").unwrap();
        app.press("id:timer_reset").unwrap();
        app.wait_for_gone("id:timer_panel", Duration::from_secs(5)).unwrap();
        app.wait_for("id:timer_open").unwrap();
        assert_eq!(app.screen().count("id:timer_indicator").unwrap(), 0);
        // Opening again shows the preset time, not a stale countdown.
        app.press("id:timer_open").unwrap();
        app.wait_for("05:00").unwrap();
    }

    #[test]
    fn landscape_layout_fills_the_screen() {
        let mut app = start_landscape();
        app.wait_for("id:music_status").unwrap();
        app.wait_for("id:camera_status").unwrap();
        let nodes = app.screen().layout_nodes();
        let find = |id: &str| {
            nodes
                .iter()
                .find(|n| n.test_id.as_deref() == Some(id))
                .unwrap_or_else(|| panic!("no node {id}"))
                .bounds
        };
        let music = find("music_status");
        let camera = find("camera_status");
        let clock = find("clock_time");
        // Music is the big content: most of the width, most of the height left under the clock.
        assert!(music.size.width.0 > 900.0, "music width {:?}", music.size);
        assert!(music.size.height.0 > 420.0, "music height {:?}", music.size);
        assert!(
            music.origin.y.0 + music.size.height.0 <= 768.0 + 0.5,
            "music passes the bottom: {music:?}"
        );
        // The camera tile is compact and sits in the top band, beside the clock.
        assert!(camera.size.height.0 < 90.0, "camera height {:?}", camera.size);
        assert!(camera.origin.y.0 < music.origin.y.0);
        assert!(camera.origin.x.0 > clock.origin.x.0 + 300.0);
    }

    #[test]
    fn music_signed_out_offers_sign_in() {
        let mut app = Headless::start(landscape_config(Root::of::<ProbeSignedOut>())).unwrap();
        app.wait_for("id:spotify_signed_out").unwrap();
        app.wait_for("Sign in to Spotify").unwrap();
        app.press("id:spotify_sign_in").unwrap();
    }

    #[test]
    fn music_awaiting_shows_code_url_and_cancel() {
        let mut app = Headless::start(landscape_config(Root::of::<ProbeAwaiting>())).unwrap();
        app.wait_for("id:spotify_awaiting").unwrap();
        app.wait_for("ABCD-1234").unwrap();
        app.wait_for("spotify.com/pair").unwrap();
        app.press("id:spotify_cancel").unwrap();
    }

    #[test]
    fn music_connecting_and_error_states_render() {
        let mut app = Headless::start(landscape_config(Root::of::<ProbeConnecting>())).unwrap();
        app.wait_for("id:spotify_connecting").unwrap();
        let mut app = Headless::start(landscape_config(Root::of::<ProbeError>())).unwrap();
        app.wait_for("id:spotify_error").unwrap();
        app.wait_for("Login expired.").unwrap();
        app.press("id:spotify_retry").unwrap();
    }

    #[test]
    fn music_connected_shows_track_and_locks_controls_when_not_allowed() {
        let mut app = Headless::start(landscape_config(Root::of::<ProbeConnectedLocked>())).unwrap();
        app.wait_for("id:spotify_connected").unwrap();
        app.wait_for("Test Song").unwrap();
        app.wait_for("Test Artist").unwrap();
        app.wait_for("Test Album").unwrap();
        app.wait_for("Playing on Windows PC").unwrap();
        app.wait_for("id:spotify_art_placeholder").unwrap();
        app.wait_for("01:01").unwrap();
        app.wait_for("03:00").unwrap();
        // Playing, so the middle button offers Pause; all three are off without control.
        app.wait_for("Pause").unwrap();
        for id in ["id:spotify_prev", "id:spotify_play_pause", "id:spotify_next"] {
            assert_eq!(app.screen().enabled(id).unwrap(), false, "{id} should be disabled");
        }
        app.wait_for("id:spotify_sign_out").unwrap();
    }

    #[test]
    fn music_connected_enables_controls_when_allowed() {
        let mut app = Headless::start(landscape_config(Root::of::<ProbeConnectedControllable>())).unwrap();
        app.wait_for("Test Song").unwrap();
        app.wait_for("Play").unwrap();
        for id in ["id:spotify_prev", "id:spotify_play_pause", "id:spotify_next"] {
            assert_eq!(app.screen().enabled(id).unwrap(), true, "{id} should be enabled");
        }
        app.press("id:spotify_play_pause").unwrap();
    }

    #[test]
    fn music_connected_with_nothing_playing_shows_empty_state() {
        let mut app = Headless::start(landscape_config(Root::of::<ProbeNothingPlaying>())).unwrap();
        app.wait_for("id:spotify_idle").unwrap();
        app.wait_for("Nothing playing").unwrap();
        assert_eq!(app.screen().count("id:spotify_title").unwrap(), 0);
    }

    #[test]
    fn portrait_still_shows_everything() {
        // The default test surface is a portrait phone; the dashboard must still render.
        let mut app = Headless::start(EngineConfig::new(Root::of::<Dashboard>())).unwrap();
        app.wait_for("id:clock_time").unwrap();
        app.wait_for("id:music_status").unwrap();
        app.wait_for("id:camera_status").unwrap();
        app.wait_for("id:timer_open").unwrap();
    }
}
