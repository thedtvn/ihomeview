//! The ihomeview dashboard root.
//!
//! A desk display for a landscape iPad (1024x768 first, portrait still works):
//!
//! * a big clock and date, with 12h/24h, Dim and a Timer button; holding the clock opens the
//!   debug log;
//! * a Music card that fills the screen and follows [`crate::spotify::SpotifyState`];
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
const DEFAULT_SECS: i64 = 15 * 60;
/// Longest settable timer: 23:59:59.
const MAX_SECS: i64 = 24 * 3600 - 1;
/// Height of one number row in a timer wheel; the wheels snap to multiples of it.
const WHEEL_ROW_PT: f32 = 44.0;
/// Rows a timer wheel shows at once (the middle one is the selected value).
const WHEEL_ROWS: f32 = 5.0;
/// Width of one timer wheel (the numbers, right-aligned) and of the "hours"/"min"/"sec" label
/// that sits beside the centered number.
const WHEEL_W_PT: f32 = 52.0;
const WHEEL_LABEL_W_PT: f32 = 46.0;
/// Width of the timer panel (and of the header column above it) in landscape.
const PANEL_W_PT: f32 = 360.0;
/// Wheel fling deceleration: velocity kept per millisecond. 0.998 is UIKit's "normal" (a flick
/// at 2 pt/ms coasts about a thousand points); 0.99 is "fast" and stops a fling within a few rows.
const WHEEL_DECELERATION: f32 = 0.998;
/// The timer panel's own background (CARD over the page), so the wheels can fade into it.
const PANEL_BG: u32 = 0x171719ff;
/// Alarm tone: pitch (Hz), one beep's length (s) and the whole clip's length (s).
const ALARM_HZ: f32 = 880.0;
const ALARM_BEEP_S: f32 = 0.16;
const ALARM_CLIP_S: f32 = 1.2;
/// The alarm repeats until dismissed, but gives up after this many clips (about 5 minutes).
const ALARM_MAX_REPEATS: u32 = 250;

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

/// Countdown text that drops leading zero units: `1:05:09`, `5:09`, `9`.
fn format_remaining(total_secs: i64) -> String {
    let t = total_secs.max(0);
    let (h, m, s) = (t / 3600, (t / 60) % 60, t % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else if m > 0 {
        format!("{m}:{s:02}")
    } else {
        format!("{s}")
    }
}

/// "4-second", "15-minute", "1:30:00" for the alert's "Your … timer has finished".
fn describe_duration(total_secs: i64) -> String {
    let (h, m, s) = split_hms(total_secs);
    match (h, m, s) {
        (0, 0, s) => format!("{s}-second"),
        (0, m, 0) => format!("{m}-minute"),
        (h, 0, 0) => format!("{h}-hour"),
        _ => format_remaining(total_secs),
    }
}

/// `(hours, minutes, seconds)` of a duration.
fn split_hms(total_secs: i64) -> (i64, i64, i64) {
    let t = total_secs.clamp(0, MAX_SECS);
    (t / 3600, (t / 60) % 60, t % 60)
}

/// Which part of the timer a drag changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TimeUnit {
    Hours,
    Minutes,
    Seconds,
}

impl TimeUnit {
    fn max(self) -> i64 {
        match self {
            TimeUnit::Hours => 23,
            TimeUnit::Minutes | TimeUnit::Seconds => 59,
        }
    }

    /// This unit's value within `total_secs`.
    fn part(self, total_secs: i64) -> i64 {
        let (h, m, s) = split_hms(total_secs);
        match self {
            TimeUnit::Hours => h,
            TimeUnit::Minutes => m,
            TimeUnit::Seconds => s,
        }
    }
}

/// A 16-bit mono WAV of two short beeps then silence, made in code so the app ships no sound file.
fn alarm_wav() -> Vec<u8> {
    const RATE: u32 = 22_050;
    let total = (ALARM_CLIP_S * RATE as f32) as usize;
    let beep = (ALARM_BEEP_S * RATE as f32) as usize;
    let gap = beep / 2;
    let mut pcm: Vec<u8> = Vec::with_capacity(total * 2);
    for i in 0..total {
        // Beep, short gap, beep, then silence for the rest of the clip.
        let in_beep = i < beep || (i >= beep + gap && i < 2 * beep + gap);
        let local = if i < beep { i } else { i.saturating_sub(beep + gap) };
        let sample = if in_beep {
            // A short fade in and out stops the clicks.
            let edge = (local.min(beep.saturating_sub(local)) as f32 / 200.0).min(1.0);
            let t = i as f32 / RATE as f32;
            ((t * ALARM_HZ * std::f32::consts::TAU).sin() * 0.6 * edge * i16::MAX as f32) as i16
        } else {
            0
        };
        pcm.extend_from_slice(&sample.to_le_bytes());
    }
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&RATE.to_le_bytes());
    wav.extend_from_slice(&(RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(&pcm);
    wav
}

/// `total_secs` with one unit moved by `steps` (clamped within that unit, the others kept).
fn adjust_unit(total_secs: i64, unit: TimeUnit, steps: i64) -> i64 {
    let (h, m, s) = split_hms(total_secs);
    let clamp = |v: i64| v.clamp(0, unit.max());
    let (h, m, s) = match unit {
        TimeUnit::Hours => (clamp(h + steps), m, s),
        TimeUnit::Minutes => (h, clamp(m + steps), s),
        TimeUnit::Seconds => (h, m, clamp(s + steps)),
    };
    h * 3600 + m * 60 + s
}

/// Track position `mm:ss`.
fn format_mm_ss(total_secs: i64) -> String {
    let t = total_secs.max(0);
    format!("{:02}:{:02}", t / 60, t % 60)
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
    format_mm_ss((ms / 1000) as i64)
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

    crate::debug_log::install();
    let twelve_hour = signal(locale_settings().hour_cycle.twelve_hour());
    let night_dim = signal(false);
    let debug_open = signal(false);

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
    // `duration_secs` is the set time (what Reset and finishing go back to).
    let duration_secs = signal(DEFAULT_SECS);
    let remaining_secs = signal(DEFAULT_SECS);
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
                    // Finished: back to the set time and raise the alarm until it is dismissed.
                    // The panel stays as it was.
                    remaining_secs.set(duration_secs.get_untracked());
                    running.set(false);
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
            scroll()
                .test_id("dashboard_scroll")
                .flex_1()
                // The page itself never moves under normal touches: no rubber band, and in
                // landscape (where everything fits on one screen) no scrolling at all. The one
                // exception is the on-screen keyboard: NSH then gives this scroll view a bottom
                // inset as tall as the keyboard and scrolls the focused field into view, which
                // keeps the Spotify login fields clear of the keyboard. Portrait may scroll when
                // its content is taller than the screen.
                .bounces(false)
                .scrollbar(false)
                .scrollable(move || !is_landscape() || keyboard_height().0 > 0.0)
                .child(
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
                        debug_open,
                        duration_secs,
                        remaining_secs,
                        running,
                        timer_open,
                    ))
                    .child(
                        view()
                            .gap(px(20.0))
                            .style_when(is_landscape, |s| s.flex_1().flex_row())
                            .child(music_card(spotify_state))
                            .child(when(timer_open, move || {
                                focus_timer_panel(duration_secs, remaining_secs, running)
                            })),
                    ),
            ),
        )
        .child(night_dim_overlay(night_dim))
        .child(when(timer_finished, move || {
            timer_alert(duration_secs, remaining_secs, running, timer_finished)
        }))
        .child(when(debug_open, move || debug_overlay(debug_open, spotify_state)))
}

// ---------------------------------------------------------------------------------------------
// Top band: clock on the left, controls on the right.
// ---------------------------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn top_band(
    clock_display: Memo<(String, &'static str)>,
    date_display: Memo<String>,
    twelve_hour: Signal<bool>,
    night_dim: Signal<bool>,
    debug_open: Signal<bool>,
    duration_secs: Signal<i64>,
    remaining_secs: Signal<i64>,
    running: Signal<bool>,
    timer_open: Signal<bool>,
) -> impl IntoElement {
    view()
        .gap(px(16.0))
        .style_when(is_landscape, |s| s.flex_row().justify_between().items_start())
        .child(clock_block(clock_display, date_display, debug_open))
        .child(
            view()
                .gap(px(12.0))
                .style_when(is_landscape, |s| s.w(px(PANEL_W_PT)))
                .child(
                    view()
                        .flex_row()
                        .flex_wrap()
                        .gap(px(10.0))
                        .style_when(is_landscape, |s| s.justify_end())
                        .child(timer_header_button(
                            duration_secs,
                            remaining_secs,
                            running,
                            timer_open,
                        ))
                        .child(hour_cycle_toggle(twelve_hour))
                        .child(night_dim_toggle(night_dim)),
                ),
        )
}

fn clock_block(
    clock_display: Memo<(String, &'static str)>,
    date_display: Memo<String>,
    debug_open: Signal<bool>,
) -> impl IntoElement {
    view()
        .test_id("clock_block")
        .hint("Hold to show the debug log")
        .on_long_press(move || debug_open.set(true))
        .long_press_delay(700.0)
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

// ---------------------------------------------------------------------------------------------
// Focus timer: header button + panel, only shown when needed.
// ---------------------------------------------------------------------------------------------

/// The one header control for the timer, which also closes the panel (there is no Close button):
/// idle: "Timer" (toggles the panel); running or paused: the remaining time (toggles the panel).
/// When the time runs out the full-screen `timer_alert` takes over; the header stays plain.
fn timer_header_button(
    duration_secs: Signal<i64>,
    remaining_secs: Signal<i64>,
    running: Signal<bool>,
    timer_open: Signal<bool>,
) -> impl IntoElement {
    // 0 idle, 1 engaged (running or paused part-way).
    let mode = computed(move || {
        if running.get() || remaining_secs.get() < duration_secs.get() {
            1u8
        } else {
            0
        }
    });
    dynamic(move || match mode.get() {
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
            .hint("Shows or hides the timer")
            .on_press(move || timer_open.update(|v| *v = !*v))
            .min_w(px(56.0))
            .min_h(px(48.0))
            .px(px(18.0))
            .items_center()
            .justify_center()
            .rounded_full()
            .border_1()
            .border_color(rgba(0xffffff22))
            .bg(move || if timer_open.get() { rgba(0xffffff2e) } else { rgba(0xffffff0f) })
            .active(|s| s.bg(rgba(0xffffff26)))
            .child(header_chip_text("Timer"))
            .into_element(),
    })
}

fn focus_timer_panel(
    duration_secs: Signal<i64>,
    remaining_secs: Signal<i64>,
    running: Signal<bool>,
) -> impl IntoElement {
    view()
        .test_id("timer_panel")
        .gap(px(16.0))
        .p(px(20.0))
        .rounded(px(24.0))
        .border_1()
        .border_color(rgba(HAIRLINE))
        .bg(rgba(CARD))
        .style_when(is_landscape, |s| s.w(px(PANEL_W_PT)))
        .child(
            text("Focus timer")
                .role(AccessibilityRole::Header)
                .text_size(px(18.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgba(MUTED)),
        )
        // Running: the countdown, centered, leading zero units hidden.
        // Stopped: the iOS-style wheel picker.
        .child(
            when(running, move || {
                text(move || format_remaining(remaining_secs.get()))
                    .test_id("timer_remaining")
                    .role(AccessibilityRole::Timer)
                    .w_full()
                    .text_center()
                    .text_size(px(72.0))
                    .font_weight(FontWeight::BOLD)
                    .tabular_nums()
                    .text_color(white())
            })
            .otherwise(move || timer_editor(duration_secs, remaining_secs)),
        )
        .child(
            view()
                .flex_row()
                .flex_wrap()
                .gap(px(8.0))
                .child((0..PRESETS_MIN.len()).map(|i| preset_button(i, duration_secs, remaining_secs, running))),
        )
        .child(
            view()
                .flex_row()
                .gap(px(10.0))
                .child(run_pause_button(remaining_secs, running))
                .child(reset_button(duration_secs, remaining_secs, running)),
        )
}

/// An iOS Clock-style picker: three wheels (hours, minutes, seconds) that you flick or drag. Each
/// wheel snaps so one number rests in the highlighted band in the middle, with its unit label
/// beside it. Numbers above and below fade out. Presets move the wheels too.
fn timer_editor(duration_secs: Signal<i64>, remaining_secs: Signal<i64>) -> impl IntoElement {
    let wheel_h = WHEEL_ROW_PT * WHEEL_ROWS;
    let fade_h = WHEEL_ROW_PT * 2.0;
    // The panel's own color, opaque at the outer edge and clear towards the band.
    let fade_from = rgba((PANEL_BG & 0xffffff00) | 0xf0);
    let fade_to = rgba(PANEL_BG & 0xffffff00);
    view()
        .test_id("timer_editor")
        .w_full()
        .h(px(wheel_h))
        // The selection band, behind the wheels, spanning all three.
        .child(
            view()
                .absolute()
                .top(px(fade_h))
                .left(px(0.0))
                .right(px(0.0))
                .h(px(WHEEL_ROW_PT))
                .rounded(px(12.0))
                .bg(rgba(0xffffff1f))
                .pointer_events(PointerEvents::None)
                .accessibility_elements_hidden(true),
        )
        .child(
            view()
                .flex_row()
                .items_center()
                .justify_center()
                .gap(px(8.0))
                .w_full()
                .h(px(wheel_h))
                .child(time_unit(TimeUnit::Hours, duration_secs, remaining_secs))
                .child(time_unit(TimeUnit::Minutes, duration_secs, remaining_secs))
                .child(time_unit(TimeUnit::Seconds, duration_secs, remaining_secs)),
        )
        // Fade the rows away from the band.
        .child(
            view()
                .absolute()
                .top(px(0.0))
                .left(px(0.0))
                .right(px(0.0))
                .h(px(fade_h))
                .bg_linear_to_b()
                .gradient_from(fade_from)
                .gradient_to(fade_to)
                .pointer_events(PointerEvents::None)
                .accessibility_elements_hidden(true),
        )
        .child(
            view()
                .absolute()
                .bottom(px(0.0))
                .left(px(0.0))
                .right(px(0.0))
                .h(px(fade_h))
                .bg_linear_to_t()
                .gradient_from(fade_from)
                .gradient_to(fade_to)
                .pointer_events(PointerEvents::None)
                .accessibility_elements_hidden(true),
        )
}

fn unit_label(unit: TimeUnit) -> &'static str {
    match unit {
        TimeUnit::Hours => "hours",
        TimeUnit::Minutes => "min",
        TimeUnit::Seconds => "sec",
    }
}

/// One wheel and its unit label, beside the centered number like Apple's.
fn time_unit(unit: TimeUnit, duration_secs: Signal<i64>, remaining_secs: Signal<i64>) -> impl IntoElement {
    view()
        .flex_row()
        .items_center()
        .child(time_wheel(unit, duration_secs, remaining_secs))
        .child(
            text(unit_label(unit))
                .w(px(WHEEL_LABEL_W_PT))
                .pl(px(2.0))
                .text_size(px(16.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(white())
                .accessibility_elements_hidden(true),
        )
}

/// A vertical scroll view of numbers `0..=max`, one row tall each, with two empty rows of room
/// above and below, so scrolling to `value * row` puts that value in the middle row.
///
/// Feel: the native scroll view does all the work while the finger is on it and while it glides
/// (flick momentum with the normal iOS deceleration, then a snap onto a row). Nothing of ours
/// runs per frame: the fade of the rows follows the scroll offset natively (`scroll_value` +
/// `interpolate`), and the time is only written when the wheel comes to rest. We scroll the wheel
/// ourselves only when the time changed from outside (a preset, VoiceOver), never in answer to
/// the wheel's own movement.
fn time_wheel(unit: TimeUnit, duration_secs: Signal<i64>, remaining_secs: Signal<i64>) -> impl IntoElement {
    let (test_id, name) = match unit {
        TimeUnit::Hours => ("timer_hours", "Hours"),
        TimeUnit::Minutes => ("timer_minutes", "Minutes"),
        TimeUnit::Seconds => ("timer_seconds", "Seconds"),
    };
    let value = computed(move || unit.part(remaining_secs.get()));
    let initial = unit.part(remaining_secs.get_untracked());
    let wheel = element_ref();
    // The row the wheel rests on (or that we sent it to). The time only differs from it when it
    // was changed from outside, and only then do we scroll the wheel.
    let shown = stored(initial);
    // The scroll offset, followed natively by the rows' fade.
    let offset = animated_value(initial as f32 * WHEEL_ROW_PT);

    effect(move || {
        let v = value.get();
        if v != shown.get() {
            shown.replace(v);
            wheel.scroll_to(point(px(0.0), px(v as f32 * WHEEL_ROW_PT)), true);
        }
    });

    let set_unit = move |idx: i64| {
        let cur = remaining_secs.get_untracked();
        let total = adjust_unit(cur, unit, idx - unit.part(cur));
        if total != cur {
            duration_secs.set(total);
            remaining_secs.set(total);
        }
    };
    // The wheel came to rest at `offset_y`: the row there is the new time.
    let commit = move |offset_y: f32| {
        let idx = ((offset_y / WHEEL_ROW_PT).round() as i64).clamp(0, unit.max());
        if idx != shown.get() {
            shown.replace(idx);
            set_unit(idx);
        }
    };

    let rows = (0..=unit.max()).map(move |i| {
        // Full strength when this row is in the band, fading to a faint number three rows away.
        let at = |k: f32| (i as f32 + k) * WHEEL_ROW_PT;
        let fade = offset.interpolate(
            &[at(-3.0), at(-2.0), at(-1.0), at(0.0), at(1.0), at(2.0), at(3.0)],
            &[0.15, 0.28, 0.55, 1.0, 0.55, 0.28, 0.15],
        );
        view()
            .w_full()
            .h(px(WHEEL_ROW_PT))
            .items_end()
            .justify_center()
            .pr(px(8.0))
            .bind_opacity(&fade)
            .child(
                text(i.to_string())
                    .text_size(px(30.0))
                    .font_weight(FontWeight::MEDIUM)
                    .tabular_nums()
                    .text_color(white()),
            )
    });

    scroll()
        .test_id(test_id)
        .w(px(WHEEL_W_PT))
        .h(px(WHEEL_ROW_PT * WHEEL_ROWS))
        .scrollbar(false)
        // Like a real picker: a hard flick may overshoot the first or last number a little.
        .bounces(true)
        .snap_interval(px(WHEEL_ROW_PT))
        .snap_alignment(SnapAlignment::Start)
        // Normal iOS deceleration, so a flick coasts over many rows before it settles on one.
        .deceleration(WHEEL_DECELERATION)
        .content_offset(point(px(0.0), px(initial as f32 * WHEEL_ROW_PT)))
        .element_ref(&wheel)
        .scroll_value(&offset)
        // Rest after a fling, or after a drag that did not fling. (A new touch that catches a
        // glide ends it with a momentum end, so that is covered too.)
        .on_scroll_end(move |at: &Point<Points>| commit(at.y.0))
        .on_momentum_end(move |at: &Point<Points>| commit(at.y.0))
        // One element for screen readers: "Hours: 2", adjustable with swipes up and down.
        .accessible(true)
        .role(AccessibilityRole::Adjustable)
        .label(move || format!("{name}: {}", value.get()))
        .accessibility_actions(vec![
            nsh_core::AccessibilityActionDef { name: "increment".into(), label: None },
            nsh_core::AccessibilityActionDef { name: "decrement".into(), label: None },
        ])
        .on_accessibility_action(move |action: &str| match action {
            "increment" => set_unit((value.get_untracked() + 1).min(unit.max())),
            "decrement" => set_unit((value.get_untracked() - 1).max(0)),
            _ => {}
        })
        .child(view().w_full().py(px(WHEEL_ROW_PT * 2.0)).child(rows))
}

fn preset_button(
    i: usize,
    duration_secs: Signal<i64>,
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
        .bg(move || if duration_secs.get() == minutes * 60 { rgba(0xffffff33) } else { rgba(0xffffff0a) })
        .on_press(move || {
            duration_secs.set(minutes * 60);
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

/// Reset stops the countdown and restores the set time; the panel stays open on the picker.
fn reset_button(
    duration_secs: Signal<i64>,
    remaining_secs: Signal<i64>,
    running: Signal<bool>,
) -> impl IntoElement {
    view()
        .role(AccessibilityRole::Button)
        .test_id("timer_reset")
        .label("Reset")
        .hint("Stops the timer and goes back to the time you set")
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
            remaining_secs.set(duration_secs.get());
        })
        .child(
            text("Reset")
                .text_size(px(18.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(white()),
        )
}

/// The "Time's up" alert: covers the dashboard, pulses amber, and rings until dismissed.
/// Dismiss closes it (the timer is already back at the set time); Restart runs the same time again.
fn timer_alert(
    duration_secs: Signal<i64>,
    remaining_secs: Signal<i64>,
    running: Signal<bool>,
    timer_finished: Signal<bool>,
) -> impl IntoElement {
    let set_time = describe_duration(duration_secs.get_untracked());
    let can_restart = duration_secs.get_untracked() > 0;

    // The whole alert fades in; the ring glows in and out until the alert is gone.
    let fade_in = motion().entering(MotionPreset::FadeIn);
    let glow = motion().entering(MotionPreset::Glow);

    // Rings until dismissed: the clip repeats. It stops with this view (closing the alert ends
    // the task and the cleanup stops a clip that is mid-play). No sound where the platform has
    // none (the headless tests) or the app is not allowed it: the alert is then visual only.
    let sound = stored(None::<Sound>);
    spawn(async move {
        match load_sound(alarm_wav()).await {
            Ok(clip) => {
                sound.replace(Some(clip.clone()));
                for _ in 0..ALARM_MAX_REPEATS {
                    clip.play(1.0, 1.0);
                    timer(Duration::from_millis((ALARM_CLIP_S * 1000.0) as u64)).await;
                }
            }
            Err(e) => crate::debug_log::warn(format!("timer alarm has no sound: {e}")),
        }
    });
    cleanup(move || {
        if let Some(Some(clip)) = sound.try_with(|s| s.take()) {
            clip.stop();
        }
    });

    let big_button = |id: &'static str, label: &'static str, hint: &'static str, filled: bool| {
        view()
            .role(AccessibilityRole::Button)
            .test_id(id)
            .label(label)
            .hint(hint)
            .min_w(px(220.0))
            .min_h(px(76.0))
            .px(px(32.0))
            .items_center()
            .justify_center()
            .rounded_full()
            .border_2()
            .border_color(rgba(AMBER))
            .bg(if filled { rgba(AMBER) } else { rgba(0xffd2781a) })
            .active(|s| s.opacity(0.75))
            .child(
                text(label)
                    .text_size(px(28.0))
                    .font_weight(FontWeight::BOLD)
                    .text_color(if filled { rgba(0x1a1203ff) } else { rgba(AMBER) }),
            )
    };

    view()
        .test_id("timer_alert")
        .role(AccessibilityRole::Alert)
        .label("Time's up")
        .absolute()
        .inset_0()
        .items_center()
        .justify_center()
        .gap(px(28.0))
        .bg(rgba(0x120c02fa))
        .bind_motion(&fade_in)
        .child(
            view()
                .size(px(220.0))
                .items_center()
                .justify_center()
                .rounded_full()
                .border(px(6.0))
                .border_color(rgba(AMBER))
                .bg(rgba(0xffd27826))
                .bind_motion(&glow)
                .accessibility_elements_hidden(true)
                .child(
                    text("0:00")
                        .text_size(px(72.0))
                        .font_weight(FontWeight::BOLD)
                        .tabular_nums()
                        .text_color(rgba(AMBER)),
                ),
        )
        .child(
            text("Time's up")
                .role(AccessibilityRole::Header)
                .text_size(px(88.0))
                .font_weight(FontWeight::BOLD)
                .text_color(white()),
        )
        .child(
            text(format!("Your {set_time} timer has finished"))
                .text_size(px(26.0))
                .text_color(rgba(MUTED)),
        )
        .child(
            view()
                .flex_row()
                .gap(px(16.0))
                .pt(px(12.0))
                .child(
                    big_button("timer_finished_dismiss", "Dismiss", "Stops the alarm", true)
                        .on_press(move || timer_finished.set(false)),
                )
                .child(when(move || can_restart, move || {
                    big_button("timer_finished_restart", "Restart", "Runs the same time again", false)
                        .on_press(move || {
                            remaining_secs.set(duration_secs.get_untracked());
                            timer_finished.set(false);
                            running.set(true);
                        })
                })),
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
        .child(state_note("Pair once with a code from your phone or computer to play, pause and skip."))
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
        text("Connecting to Spotify\u{2026}")
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

// ---------------------------------------------------------------------------------------------
// Debug log overlay: hold the clock to open it.
// ---------------------------------------------------------------------------------------------

fn debug_button(test_id: &'static str, label: &'static str, on_press: impl Fn() + 'static) -> impl IntoElement {
    view()
        .role(AccessibilityRole::Button)
        .test_id(test_id)
        .label(label)
        .on_press(on_press)
        .min_h(px(44.0))
        .px(px(18.0))
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .border_color(rgba(0xffffff33))
        .bg(rgba(0xffffff12))
        .active(|s| s.bg(rgba(0xffffff2a)))
        .child(text(label).text_size(px(16.0)).font_weight(FontWeight::SEMIBOLD).text_color(white()))
}

/// Newest lines first, so the latest step is always at the top without scrolling.
fn newest_first(log: &str) -> String {
    let mut lines: Vec<&str> = log.lines().collect();
    lines.reverse();
    lines.join("\n")
}

fn debug_overlay(debug_open: Signal<bool>, spotify_state: Signal<SpotifyState>) -> impl IntoElement {
    let log_text = signal(newest_first(&crate::debug_log::text()));
    let seen = stored(crate::debug_log::version());
    let copied = signal(false);
    set_interval(Duration::from_millis(500), move || {
        let v = crate::debug_log::version();
        if v != seen.get() {
            seen.replace(v);
            log_text.set(newest_first(&crate::debug_log::text()));
        }
    });
    let state_line = move || {
        let state = match spotify_state.get() {
            SpotifyState::Connected(p) => format!(
                "Connected (device {:?}, playing {}, can control {}, track {:?})",
                p.device, p.is_playing, p.can_control, p.title
            ),
            other => format!("{other:?}"),
        };
        format!("Spotify state: {state}")
    };
    view()
        .test_id("debug_overlay")
        .absolute()
        .inset_0()
        .bg(rgba(0x000000f2))
        .p(px(24.0))
        .gap(px(12.0))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(10.0))
                .child(
                    text("Debug log")
                        .role(AccessibilityRole::Header)
                        .flex_1()
                        .text_size(px(28.0))
                        .font_weight(FontWeight::BOLD)
                        .text_color(white()),
                )
                .child(debug_button("debug_copy", "Copy", move || {
                    let mut all = state_line();
                    all.push('\n');
                    all.push_str(&crate::debug_log::text());
                    set_clipboard(all);
                    copied.set(true);
                }))
                .child(debug_button("debug_clear", "Clear", move || {
                    crate::debug_log::clear();
                    crate::debug_log::info("log cleared");
                }))
                .child(debug_button("debug_close", "Close", move || debug_open.set(false))),
        )
        .child(
            text(state_line)
                .test_id("debug_state")
                .text_size(px(15.0))
                .text_color(rgba(GREEN)),
        )
        .child(
            text(move || {
                format!(
                    "{} lines, newest first{}{}",
                    crate::debug_log::line_count(),
                    if crate::debug_log::capturing_crates() { "" } else { " (librespot lines not captured)" },
                    if copied.get() { " - copied to clipboard" } else { "" }
                )
            })
            .text_size(px(13.0))
            .text_color(rgba(MUTED)),
        )
        .child(
            scroll()
                .flex_1()
                .rounded(px(12.0))
                .bg(rgba(0xffffff0a))
                .child(
                    text(move || log_text.get())
                        .test_id("debug_log_text")
                        .p(px(12.0))
                        .text_size(px(13.0))
                        .line_height(px(18.0))
                        .text_color(rgba(0xffffffdd)),
                ),
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
    fn remaining_time_hides_zero_hours_and_minutes() {
        assert_eq!(format_remaining(3 * 3600 + 5 * 60 + 9), "3:05:09");
        assert_eq!(format_remaining(3600), "1:00:00");
        assert_eq!(format_remaining(65), "1:05");
        assert_eq!(format_remaining(15 * 60), "15:00");
        assert_eq!(format_remaining(59), "59");
        assert_eq!(format_remaining(0), "0");
        assert_eq!(format_remaining(-5), "0");
    }

    #[test]
    fn adjusting_a_unit_keeps_the_others_and_clamps() {
        let t = 1 * 3600 + 30 * 60 + 15;
        assert_eq!(adjust_unit(t, TimeUnit::Hours, 2), 3 * 3600 + 30 * 60 + 15);
        assert_eq!(adjust_unit(t, TimeUnit::Minutes, 40), 1 * 3600 + 59 * 60 + 15);
        assert_eq!(adjust_unit(t, TimeUnit::Seconds, -20), 1 * 3600 + 30 * 60);
        assert_eq!(adjust_unit(t, TimeUnit::Hours, -5), 30 * 60 + 15);
        assert_eq!(adjust_unit(0, TimeUnit::Hours, 99), 23 * 3600);
    }

    #[test]
    fn progress_fraction_is_clamped() {
        assert_eq!(progress_fraction(0, 0), 0.0);
        assert_eq!(progress_fraction(50, 100), 0.5);
        assert_eq!(progress_fraction(500, 100), 1.0);
        assert_eq!(format_track_time(61_000), "01:01");
    }

    #[test]
    fn dashboard_shows_clock_and_music_sign_in_without_camera() {
        let mut app = start_landscape();
        app.wait_for("id:clock_time").unwrap();
        app.wait_for("id:music_status").unwrap();
        assert_eq!(app.screen().count("id:camera_status").unwrap(), 0);
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

        // The same Timer button opens and closes the panel; there is no Close button.
        app.press("id:timer_open").unwrap();
        app.wait_for("id:timer_editor").unwrap();
        assert_eq!(app.screen().count("id:timer_close").unwrap(), 0);
        app.press("id:timer_open").unwrap();
        app.wait_for_gone("id:timer_panel", Duration::from_secs(5)).unwrap();
        app.press("id:timer_open").unwrap();
        app.wait_for("id:timer_panel").unwrap();

        app.press("id:timer_toggle").unwrap();
        app.wait_for("id:timer_indicator").unwrap();
        app.wait_for("id:timer_remaining").unwrap();
        assert_eq!(app.screen().count("id:timer_open").unwrap(), 0);

        // The indicator closes the panel and keeps counting.
        app.press("id:timer_indicator").unwrap();
        app.wait_for_gone("id:timer_panel", Duration::from_secs(5)).unwrap();
        app.wait_for("id:timer_indicator").unwrap();

        // Tapping the indicator expands it again.
        app.press("id:timer_indicator").unwrap();
        app.wait_for("id:timer_panel").unwrap();
    }

    #[test]
    fn timer_reset_restores_time_and_keeps_the_panel_open() {
        let mut app = start_landscape();
        app.wait_for("id:timer_open").unwrap();
        app.press("id:timer_open").unwrap();
        app.wait_for("id:timer_preset_5").unwrap();
        app.press("id:timer_preset_5").unwrap();
        app.press("id:timer_toggle").unwrap();
        app.wait_for("5:00").unwrap();
        app.wait_for("id:timer_indicator").unwrap();
        assert_eq!(app.screen().count("id:timer_editor").unwrap(), 0);
        app.press("id:timer_reset").unwrap();
        // The panel stays open and shows the picker again; the header indicator is gone.
        app.wait_for("id:timer_editor").unwrap();
        app.wait_for("id:timer_open").unwrap();
        assert_eq!(app.screen().count("id:timer_panel").unwrap(), 1);
        assert_eq!(app.screen().count("id:timer_indicator").unwrap(), 0);
        assert_eq!(app.screen().count("id:timer_remaining").unwrap(), 0);
        // The wheels show the set time (5 minutes), not a stale countdown.
        app.wait_for("label:Minutes: 5").unwrap();
        app.wait_for("label:Seconds: 0").unwrap();
    }

    #[test]
    fn presets_move_the_wheels_and_the_wheels_set_the_time() {
        let mut app = start_landscape();
        app.wait_for("id:timer_open").unwrap();
        app.press("id:timer_open").unwrap();
        app.wait_for("id:timer_editor").unwrap();
        // Three wheels, each with its test id and an accessible "Name: value" label.
        for id in ["id:timer_hours", "id:timer_minutes", "id:timer_seconds"] {
            app.wait_for(id).unwrap();
        }
        app.wait_for("label:Hours: 0").unwrap();
        app.wait_for("label:Minutes: 15").unwrap();
        app.wait_for("label:Seconds: 0").unwrap();
        // A preset changes what the wheels show.
        app.press("id:timer_preset_25").unwrap();
        app.wait_for("label:Minutes: 25").unwrap();

        // Scrolling a wheel by whole rows picks a value: 2 rows down the hours wheel is 2 hours.
        // (Headless moves the scroll view's offset and fires its scroll events; it does not
        // simulate the native snap, so this lands exactly on a row.)
        app.scroll("id:timer_hours", px(2.0 * WHEEL_ROW_PT)).unwrap();
        app.wait_for("label:Hours: 2").unwrap();
        app.wait_for("label:Minutes: 25").unwrap();
        app.press("id:timer_toggle").unwrap();
        app.wait_for("id:timer_remaining").unwrap();
        app.wait_for("2:25:00").unwrap();
    }

    #[test]
    fn dragging_a_wheel_changes_the_value() {
        let mut app = start_landscape();
        app.wait_for("id:timer_open").unwrap();
        app.press("id:timer_open").unwrap();
        app.wait_for("label:Minutes: 15").unwrap();
        // A finger drag up by 3 rows moves the minutes wheel to 18 (a drag up scrolls down).
        app.drag("id:timer_minutes", nsh_core::geometry::point(px(0.0), px(-(3.0 * WHEEL_ROW_PT)))).unwrap();
        app.wait_for("label:Minutes: 18").unwrap();
        app.press("id:timer_toggle").unwrap();
        app.wait_for("id:timer_remaining").unwrap();
        app.wait_for("18:00").unwrap();
    }

    #[test]
    fn the_running_countdown_is_centered_in_the_panel() {
        let mut app = start_landscape();
        app.wait_for("id:timer_open").unwrap();
        app.press("id:timer_open").unwrap();
        app.wait_for("id:timer_toggle").unwrap();
        app.press("id:timer_toggle").unwrap();
        app.wait_for("id:timer_remaining").unwrap();
        let nodes = app.screen().layout_nodes();
        let find = |id: &str| {
            nodes
                .iter()
                .find(|n| n.test_id.as_deref() == Some(id))
                .unwrap_or_else(|| panic!("no node {id}"))
                .bounds
        };
        let panel = find("timer_panel");
        let remaining = find("timer_remaining");
        let panel_mid = panel.origin.x.0 + panel.size.width.0 / 2.0;
        let text_mid = remaining.origin.x.0 + remaining.size.width.0 / 2.0;
        assert!((panel_mid - text_mid).abs() < 2.0, "countdown box not centered: {panel_mid} vs {text_mid}");
    }

    #[test]
    fn time_up_raises_an_alert_until_dismissed() {
        let mut app = start_landscape();
        app.wait_for("id:timer_open").unwrap();
        app.press("id:timer_open").unwrap();
        app.wait_for("label:Minutes: 15").unwrap();
        // Set 0:02: minutes wheel back to 0, seconds wheel to 2.
        app.scroll("id:timer_minutes", px(-15.0 * WHEEL_ROW_PT)).unwrap();
        app.wait_for("label:Minutes: 0").unwrap();
        app.scroll("id:timer_seconds", px(2.0 * WHEEL_ROW_PT)).unwrap();
        app.wait_for("label:Seconds: 2").unwrap();
        assert_eq!(app.screen().count("id:timer_alert").unwrap(), 0);
        app.press("id:timer_toggle").unwrap();
        app.wait_for("id:timer_remaining").unwrap();

        // Time runs out: a big alert appears with Dismiss and Restart; the panel stays open.
        app.wait_for_within("id:timer_finished_dismiss", Duration::from_secs(8)).unwrap();
        app.wait_for("id:timer_alert").unwrap();
        app.wait_for("id:timer_finished_restart").unwrap();
        assert_eq!(app.screen().count("id:timer_panel").unwrap(), 1);
        assert_eq!(app.screen().count("id:timer_remaining").unwrap(), 0);
        assert_eq!(app.screen().count("id:timer_indicator").unwrap(), 0);

        // Dismiss: the alert goes, the picker shows the set time again.
        app.press("id:timer_finished_dismiss").unwrap();
        app.wait_for_gone("id:timer_alert", Duration::from_secs(5)).unwrap();
        app.wait_for("id:timer_editor").unwrap();
        app.wait_for("label:Seconds: 2").unwrap();
    }

    #[test]
    fn restart_from_the_alert_runs_the_same_time_again() {
        let mut app = start_landscape();
        app.wait_for("id:timer_open").unwrap();
        app.press("id:timer_open").unwrap();
        app.wait_for("label:Minutes: 15").unwrap();
        app.scroll("id:timer_minutes", px(-15.0 * WHEEL_ROW_PT)).unwrap();
        app.scroll("id:timer_seconds", px(2.0 * WHEEL_ROW_PT)).unwrap();
        app.wait_for("label:Seconds: 2").unwrap();
        app.press("id:timer_toggle").unwrap();
        app.wait_for_within("id:timer_finished_restart", Duration::from_secs(8)).unwrap();
        app.press("id:timer_finished_restart").unwrap();
        app.wait_for_gone("id:timer_alert", Duration::from_secs(5)).unwrap();
        app.wait_for("id:timer_remaining").unwrap();
    }

    #[test]
    fn holding_the_clock_opens_the_debug_log() {
        let mut app = start_landscape();
        app.wait_for("id:clock_block").unwrap();
        assert_eq!(app.screen().count("id:debug_overlay").unwrap(), 0);
        crate::debug_log::info("test line for overlay");
        app.long_press("id:clock_block").unwrap();
        app.wait_for("id:debug_overlay").unwrap();
        app.wait_for("Spotify state: SignedOut").unwrap();
        app.press("id:debug_close").unwrap();
        app.wait_for_gone("id:debug_overlay", Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn landscape_layout_fills_the_screen() {
        let mut app = start_landscape();
        app.wait_for("id:music_status").unwrap();
        app.wait_for("id:night_dim_toggle").unwrap();
        let nodes = app.screen().layout_nodes();
        let find = |id: &str| {
            nodes
                .iter()
                .find(|n| n.test_id.as_deref() == Some(id))
                .unwrap_or_else(|| panic!("no node {id}"))
                .bounds
        };
        let music = find("music_status");
        let controls = find("night_dim_toggle");
        let clock = find("clock_time");
        // Music is the big content: most of the width, most of the height left under the clock.
        assert!(music.size.width.0 > 900.0, "music width {:?}", music.size);
        assert!(music.size.height.0 > 420.0, "music height {:?}", music.size);
        assert!(
            music.origin.y.0 + music.size.height.0 <= 768.0 + 0.5,
            "music passes the bottom: {music:?}"
        );
        // The header controls sit in the top band, beside the clock.
        assert!(controls.origin.y.0 < music.origin.y.0);
        assert!(controls.origin.x.0 > clock.origin.x.0 + 300.0);
    }

    #[test]
    fn music_signed_out_offers_sign_in() {
        let mut app = Headless::start(landscape_config(Root::of::<ProbeSignedOut>())).unwrap();
        app.wait_for("id:spotify_signed_out").unwrap();
        app.wait_for("Sign in to Spotify").unwrap();
        assert!(app.screen().enabled("id:spotify_sign_in").unwrap());
        assert_eq!(app.screen().count("id:spotify_password").unwrap(), 0);
    }

    /// Regression: the sign-in button is disposed as soon as the state becomes `Connecting`;
    /// the sign-in task must outlive it (it used to be cancelled before it ran).
    #[test]
    fn sign_in_task_outlives_the_button() {
        let mut app = start_landscape();
        app.wait_for("id:spotify_sign_in").unwrap();
        app.press("id:spotify_sign_in").unwrap();
        let started = (0..50).any(|_| {
            if crate::debug_log::text().contains("sign-in: pairing task started")
                || crate::debug_log::text().contains("pairing code request failed")
            {
                return true;
            }
            let _ = app.wait_for_gone("id:never_there", Duration::from_millis(40));
            false
        });
        assert!(started, "sign-in task never ran:\n{}", crate::debug_log::text());
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
    fn landscape_dashboard_does_not_move_under_touches() {
        let mut app = start_landscape();
        app.wait_for("id:clock_time").unwrap();
        app.wait_for("id:dashboard_scroll").unwrap();
        // The page is not user-scrollable in landscape (no keyboard up), so dragging it fails.
        let err = app.scroll("id:dashboard_scroll", px(200.0)).unwrap_err();
        eprintln!("expected refusal: {err}");
        // A finger drag across the clock leaves everything where it was.
        let before = app.screen().layout_nodes();
        let top = |nodes: &[nsh_testing::LayoutNode]| {
            nodes
                .iter()
                .find(|n| n.test_id.as_deref() == Some("clock_time"))
                .unwrap()
                .bounds
                .origin
                .y
                .0
        };
        let y0 = top(&before);
        app.drag("id:clock_time", nsh_core::geometry::point(px(0.0), px(-150.0))).unwrap();
        let after = app.screen().layout_nodes();
        assert_eq!(y0, top(&after), "the dashboard moved under a drag");
    }

    #[test]
    fn portrait_still_shows_everything() {
        // The default test surface is a portrait phone; the dashboard must still render.
        let mut app = Headless::start(EngineConfig::new(Root::of::<Dashboard>())).unwrap();
        app.wait_for("id:clock_time").unwrap();
        app.wait_for("id:music_status").unwrap();
        app.wait_for("id:timer_open").unwrap();
    }
}
