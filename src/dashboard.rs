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

use std::rc::Rc;
use std::time::Duration;

use chrono::{Datelike, Local, Timelike};
use nsh_base::theme;
use nsh_core::{Checked, prelude::*, time::unix_ms};

use crate::spotify::{
    self, Playback, PlaylistSummary, RepeatMode, SaveTrackState, SpotifyCommand, SpotifyState,
};

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
    // Spotify: the backend owns the state, the UI only reads it and sends commands.
    let spotify_state = signal(SpotifyState::SignedOut);
    spotify::start(spotify_state);
    dashboard_content(spotify_state, spotify::playlists(), MusicHooks::live(spotify_state), false)
}

/// The whole screen for a given Spotify state and playlist list (the tests drive it directly).
fn dashboard_content(
    spotify_state: Signal<SpotifyState>,
    playlists: Signal<Vec<PlaylistSummary>>,
    hooks: MusicHooks,
    timer_initially_open: bool,
) -> impl IntoElement {
    // The add-to-playlist sheet closes if Spotify goes away under it (sign out, error).
    let add_target = hooks.target;
    let connected = computed(move || spotify_kind(&spotify_state.get()) == 4);
    effect(move || {
        if !connected.get() && add_target.get_untracked().is_some() {
            add_target.set(None);
        }
    });
    let sheet_hooks = hooks.clone();

    // Clock: one tick per second, aligned just after each second boundary (battery: half the
    // wakeups of a 500ms poll, and the display only changes once a second anyway).
    // The task ends with this view, per https://nsh.dev/docs/reactivity.md#cleanup.
    let now_local = signal(Local::now());
    spawn(async move {
        loop {
            let ms = Local::now().timestamp_subsec_millis() as u64;
            timer(Duration::from_millis(1000 - ms.min(999) + 5)).await;
            now_local.set(Local::now());
        }
    });

    crate::debug_log::install();
    let twelve_hour = signal(locale_settings().hour_cycle.twelve_hour());
    let night_dim = signal(false);
    let debug_open = signal(false);
    let settings_open = signal(false);

    let clock_display = computed(move || {
        let dt = now_local.get();
        format_clock(dt.hour(), dt.minute(), dt.second(), twelve_hour.get())
    });
    let date_display = computed(move || {
        let dt = now_local.get();
        format_date(&dt)
    });

    // Focus timer state. Hidden until `timer_open`; see `timer_header_button`.
    // `duration_secs` is the set time (what Reset and finishing go back to).
    let duration_secs = signal(DEFAULT_SECS);
    let remaining_secs = signal(DEFAULT_SECS);
    let running = signal(false);
    let timer_open = signal(timer_initially_open);
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
                        night_dim,
                        debug_open,
                        settings_open,
                        duration_secs,
                        remaining_secs,
                        running,
                        timer_open,
                    ))
                    .child(
                        // Landscape: the Music card and the timer panel sit side by side. The
                        // panel has a fixed width and never shrinks; the card takes what is left
                        // and rearranges itself (`music_card`) so nothing leaves it.
                        view()
                            .gap(px(20.0))
                            .style_when(is_landscape, |s| s.flex_1().flex_row().min_h(px(0.0)))
                            .child(music_card(spotify_state, playlists, hooks))
                            .child(when(timer_open, move || {
                                focus_timer_panel(duration_secs, remaining_secs, running)
                            })),
                    ),
            ),
        )
        .child(when(settings_open, move || {
            settings_menu(settings_open, night_dim, twelve_hour, debug_open, spotify_state)
        }))
        .child(when(move || add_target.get().is_some(), move || {
            add_sheet(add_target, playlists, sheet_hooks.clone())
        }))
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
    night_dim: Signal<bool>,
    debug_open: Signal<bool>,
    settings_open: Signal<bool>,
    duration_secs: Signal<i64>,
    remaining_secs: Signal<i64>,
    running: Signal<bool>,
    timer_open: Signal<bool>,
) -> impl IntoElement {
    view()
        .gap(px(16.0))
        .style_when(is_landscape, |s| s.flex_row().justify_between().items_start())
        .child(clock_block(clock_display, date_display, debug_open))
        // The header is just two controls: Timer and Settings.
        .child(
            view()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap(px(10.0))
                .child(timer_header_button(duration_secs, remaining_secs, running, timer_open))
                .child(settings_button(settings_open, night_dim)),
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

// ---------------------------------------------------------------------------------------------
// Icons: small vector glyphs drawn with NSH's canvas (no font, no image files, no SVG feature).
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Icon {
    Previous,
    Next,
    Play,
    Pause,
    Shuffle,
    /// Shuffle with a sparkle, like Spotify's Smart Shuffle.
    SmartShuffle,
    Repeat,
    /// Repeat with a "1" in the middle: repeat the current track.
    RepeatOne,
    Gear,
    Note,
    /// Liked Songs.
    Heart,
    Plus,
    Check,
    Search,
    /// Speaker with no waves and a cross: muted.
    VolumeOff,
    /// Speaker with one wave.
    VolumeLow,
    /// Speaker with two waves.
    VolumeHigh,
    /// Spotify DJ: rings broken into arcs around a dot, drawn in one colour.
    Dj,
}

// Path data on a 24x24 grid.
const PATH_PREVIOUS: &str = "M6 5h2.4v14H6z M9.6 12 19 5v14z";
const PATH_NEXT: &str = "M15.6 5H18v14h-2.4z M14.4 12 5 5v14z";
const PATH_PLAY: &str = "M8 5v14l11-7z";
const PATH_PAUSE: &str = "M6.5 5h4v14h-4z M13.5 5h4v14h-4z";
const PATH_SHUFFLE: &str = "M3 7h3.5c2 0 3.2 1 4.3 2.6l2.4 3.8c1.1 1.6 2.3 2.6 4.3 2.6H21 \
     M3 17h3.5c2 0 3.2-1 4.3-2.6 M21 7h-3.5c-1.6 0-2.7.6-3.6 1.6 \
     M18 4l3 3-3 3 M18 14l3 3-3 3";
const PATH_SPARKLE: &str = "M19.5 .5 20.8 3.2 23.5 4.5 20.8 5.8 19.5 8.5 18.2 5.8 15.5 4.5 18.2 3.2z";
const PATH_REPEAT: &str = "M17 2l4 4-4 4 M3 11v-1a4 4 0 0 1 4-4h14 M7 22l-4-4 4-4 M21 13v1a4 4 0 0 1-4 4H3";
const PATH_ONE: &str = "M10.6 10.2 12.2 9v6";
const PATH_NOTE: &str = "M10 3v11.4a3.5 3.5 0 1 0 2 3.1V7h6V3z";
const PATH_HEART: &str = "M12 21 C5 15.4 2 11.8 2 8.2 C2 5.2 4.3 3 7.1 3 C9.1 3 11 4.1 12 5.9 \
     C13 4.1 14.9 3 16.9 3 C19.7 3 22 5.2 22 8.2 C22 11.8 19 15.4 12 21 Z";
const PATH_PLUS: &str = "M12 5v14 M5 12h14";
const PATH_CHECK: &str = "M5 12.8l4.6 4.6L19 7.6";
const PATH_SEARCH: &str = "M10.5 3.5a7 7 0 1 0 0 14 7 7 0 0 0 0-14z M16 16l4.5 4.5";
const PATH_SPEAKER: &str = "M3 9.5v5h3.8l5.2 4.4V5.1L6.8 9.5z";
const PATH_WAVE_LOW: &str = "M15.6 9a4.2 4.2 0 0 1 0 6";
const PATH_WAVE_HIGH: &str = "M18.4 6.2a8 8 0 0 1 0 11.6";
const PATH_MUTE_CROSS: &str = "M16 9.5l5 5 M21 9.5l-5 5";

fn icon_painter(icon: Icon, size: f32, color: u32) -> Painter {
    let c = rgba(color);
    let p = Painter::new()
        .save()
        .scale(size / 24.0, size / 24.0)
        .fill_style(c)
        .stroke_style(c)
        .line_width(2.0_f32)
        .line_cap(nsh_core::canvas::LineCap::Round)
        .line_join(nsh_core::canvas::LineJoin::Round);
    let p = match icon {
        Icon::Previous => p.fill_svg(PATH_PREVIOUS),
        Icon::Next => p.fill_svg(PATH_NEXT),
        Icon::Play => p.fill_svg(PATH_PLAY),
        Icon::Pause => p.fill_svg(PATH_PAUSE),
        Icon::Shuffle => p.stroke_svg(PATH_SHUFFLE),
        Icon::SmartShuffle => p
            .save()
            .translate(0.0_f32, 5.0_f32)
            .scale(0.78_f32, 0.78_f32)
            .stroke_svg(PATH_SHUFFLE)
            .restore()
            .fill_svg(PATH_SPARKLE),
        Icon::Repeat => p.stroke_svg(PATH_REPEAT),
        Icon::RepeatOne => p.stroke_svg(PATH_REPEAT).line_width(1.7_f32).stroke_svg(PATH_ONE),
        Icon::Note => p.fill_svg(PATH_NOTE),
        Icon::Heart => p.fill_svg(PATH_HEART),
        Icon::Plus => p.line_width(2.4_f32).stroke_svg(PATH_PLUS),
        Icon::Check => p.line_width(2.8_f32).stroke_svg(PATH_CHECK),
        Icon::Search => p.line_width(2.2_f32).stroke_svg(PATH_SEARCH),
        Icon::VolumeOff => p.fill_svg(PATH_SPEAKER).stroke_svg(PATH_MUTE_CROSS),
        Icon::VolumeLow => p.fill_svg(PATH_SPEAKER).stroke_svg(PATH_WAVE_LOW),
        Icon::VolumeHigh => p
            .fill_svg(PATH_SPEAKER)
            .stroke_svg(PATH_WAVE_LOW)
            .stroke_svg(PATH_WAVE_HIGH),
        Icon::Dj => {
            // Three broken rings turned against each other, and a solid dot in the middle.
            let ring = |g: Painter, r: f32, from: f32, to: f32| {
                g.begin_path().arc(0.0_f32, 0.0_f32, r, deg(from), deg(to)).stroke()
            };
            let g = p.translate(12.0_f32, 12.0_f32).line_width(2.1_f32);
            let g = ring(g, 10.4, 200.0, 330.0);
            let g = ring(g, 10.4, 20.0, 150.0);
            let g = ring(g, 7.0, 290.0, 60.0 + 360.0);
            let g = ring(g, 7.0, 100.0, 250.0);
            let g = ring(g, 3.9, -30.0, 120.0);
            let g = ring(g, 3.9, 150.0, 300.0);
            g.begin_path().arc(0.0_f32, 0.0_f32, 1.1_f32, deg(0.0), deg(360.0)).fill()
        }
        Icon::Gear => {
            // A ring with eight teeth around it.
            let mut g = p
                .translate(12.0_f32, 12.0_f32)
                .line_width(3.8_f32)
                .begin_path()
                .arc(0.0_f32, 0.0_f32, 6.4_f32, deg(0.0), deg(360.0))
                .stroke();
            for i in 0..8 {
                g = g
                    .save()
                    .rotate(deg(45.0 * i as f32))
                    .fill_rect(-1.9_f32, -11.6_f32, 3.8_f32, 4.6_f32)
                    .restore();
            }
            g
        }
    };
    p.restore()
}

/// An icon `size` points square, drawn in `color` (re-drawn when what `color` reads changes).
fn icon(kind: impl Fn() -> Icon + 'static, size: f32, color: impl Fn() -> u32 + 'static) -> CanvasView {
    canvas(move || icon_painter(kind(), size, color()))
        .w(px(size))
        .h(px(size))
        .flex_shrink_0()
        .pointer_events(PointerEvents::None)
        .accessibility_elements_hidden(true)
}

// ---------------------------------------------------------------------------------------------
// Settings: a gear button in the header (tap = menu, hold = night dim) and the menu it opens.
// ---------------------------------------------------------------------------------------------

fn settings_button(settings_open: Signal<bool>, night_dim: Signal<bool>) -> impl IntoElement {
    view()
        .role(AccessibilityRole::Button)
        .test_id("settings_button")
        .label("Settings")
        .hint("Opens the settings. Hold to turn night dim on or off.")
        .on_press(move || settings_open.update(|v| *v = !*v))
        .on_long_press(move || night_dim.update(|v| *v = !*v))
        .long_press_delay(600.0)
        .w(px(52.0))
        .h(px(52.0))
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .border_color(move || if night_dim.get() { rgba(0xffd27888) } else { rgba(0xffffff22) })
        .bg(move || {
            if settings_open.get() {
                rgba(0xffffff2e)
            } else if night_dim.get() {
                rgba(0xffd27833)
            } else {
                rgba(0xffffff0f)
            }
        })
        .active(|s| s.bg(rgba(0xffffff26)))
        .child(icon(|| Icon::Gear, 26.0, move || if night_dim.get() { AMBER } else { 0xffffffff }))
}

/// A row with a title and an on/off switch.
fn settings_switch(
    test_id: &'static str,
    title: &'static str,
    detail: &'static str,
    on: impl Fn() -> bool + Copy + 'static,
    toggle: impl Fn() + 'static,
) -> impl IntoElement {
    view()
        .role(AccessibilityRole::Switch)
        .test_id(test_id)
        .label(title)
        .accessibility_state(move || AccessibilityState {
            checked: Some(if on() { Checked::True } else { Checked::False }),
            ..AccessibilityState::default()
        })
        .on_press(move || toggle())
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(12.0))
        .min_h(px(60.0))
        .px(px(14.0))
        .rounded(px(14.0))
        .active(|s| s.bg(rgba(0xffffff12)))
        .child(
            view()
                .flex_1()
                .min_w(px(0.0))
                .child(text(title).text_size(px(19.0)).font_weight(FontWeight::MEDIUM).text_color(white()))
                .child(text(detail).text_size(px(14.0)).text_color(rgba(FAINT))),
        )
        .child(
            view()
                .w(px(52.0))
                .h(px(32.0))
                .p(px(3.0))
                .flex_row()
                .items_center()
                .flex_shrink_0()
                .rounded_full()
                .bg(move || if on() { rgba(GREEN) } else { rgba(0xffffff2a) })
                .style_with(move |s| if on() { s.justify_end() } else { s.justify_start() })
                .child(view().size(px(26.0)).rounded_full().bg(white())),
        )
}

/// A row with a title (and a small note) and a pill button on the right.
fn settings_action(
    row_id: &'static str,
    title: &'static str,
    note: impl Fn() -> String + 'static,
    button: Option<(&'static str, &'static str, bool, Box<dyn Fn()>)>,
) -> impl IntoElement {
    let row = view()
        .test_id(row_id)
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(12.0))
        .min_h(px(60.0))
        .px(px(14.0))
        .child(
            view()
                .flex_1()
                .min_w(px(0.0))
                .child(text(title).text_size(px(19.0)).font_weight(FontWeight::MEDIUM).text_color(white()))
                .child(
                    text(note)
                        .text_size(px(14.0))
                        .number_of_lines(1)
                        .text_color(rgba(FAINT)),
                ),
        );
    match button {
        Some((id, label, danger, on_press)) => row
            .child(
                view()
                    .role(AccessibilityRole::Button)
                    .test_id(id)
                    .label(label)
                    .on_press(move || on_press())
                    .min_h(px(44.0))
                    .px(px(18.0))
                    .flex_shrink_0()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .border_1()
                    .border_color(if danger { rgba(0xff6b6b88) } else { rgba(0xffffff33) })
                    .bg(if danger { rgba(0xff6b6b22) } else { rgba(0xffffff12) })
                    .active(|s| s.opacity(0.75))
                    .child(
                        text(label)
                            .text_size(px(16.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(if danger { rgba(0xff8d8dff) } else { rgba(0xffffffff) }),
                    ),
            )
            .into_element(),
        None => row.into_element(),
    }
}

/// The menu the gear opens: a card under the header, with a transparent backdrop that closes it.
fn settings_menu(
    settings_open: Signal<bool>,
    night_dim: Signal<bool>,
    twelve_hour: Signal<bool>,
    debug_open: Signal<bool>,
    spotify_state: Signal<SpotifyState>,
) -> impl IntoElement {
    // 0 signed out / problem, 1 signed in, 2 working on it.
    let account = computed(move || match spotify_state.get() {
        SpotifyState::Connected(_) => 1u8,
        SpotifyState::Connecting | SpotifyState::AwaitingApproval { .. } => 2,
        _ => 0,
    });
    let device_note = move || match spotify_state.get() {
        SpotifyState::Connected(p) => match p.device {
            Some(d) => format!("Signed in, playing on {d}"),
            None => "Signed in".to_string(),
        },
        SpotifyState::Connecting | SpotifyState::AwaitingApproval { .. } => "Signing in\u{2026}".to_string(),
        SpotifyState::Unavailable(_) => "Not available in this build".to_string(),
        SpotifyState::Error(_) => "Sign-in problem".to_string(),
        SpotifyState::SignedOut => "Not signed in".to_string(),
    };
    view()
        .test_id("settings_menu")
        .absolute()
        .inset_0()
        .child(
            view()
                .test_id("settings_backdrop")
                .role(AccessibilityRole::Button)
                .label("Close settings")
                .absolute()
                .inset_0()
                .on_press(move || settings_open.set(false)),
        )
        .child(
            view()
                .test_id("settings_panel")
                .role(AccessibilityRole::Menu)
                .absolute()
                .top(px(92.0))
                .right(px(24.0))
                .w(px(360.0))
                .p(px(10.0))
                .gap(px(2.0))
                .rounded(px(24.0))
                .border_1()
                .border_color(rgba(0xffffff26))
                .bg(rgba(0x1b1b1eff))
                .shadow_2xl()
                .child(
                    text("Settings")
                        .role(AccessibilityRole::Header)
                        .px(px(14.0))
                        .py(px(8.0))
                        .text_size(px(16.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgba(MUTED)),
                )
                .child(settings_switch(
                    "settings_night_dim",
                    "Night dim",
                    "Darker screen. Hold the gear to toggle.",
                    move || night_dim.get(),
                    move || night_dim.update(|v| *v = !*v),
                ))
                .child(settings_switch(
                    "settings_24h",
                    "24-hour clock",
                    "Off shows AM and PM.",
                    move || !twelve_hour.get(),
                    move || twelve_hour.update(|v| *v = !*v),
                ))
                .child(settings_action(
                    "settings_keep_awake_row",
                    "Keep screen on",
                    || match crate::keep_awake::mode().get() {
                        crate::keep_awake::KeepAwake::WhileCharging => "Only while plugged in (saves battery)".to_string(),
                        crate::keep_awake::KeepAwake::Always => "Always, even on battery".to_string(),
                        crate::keep_awake::KeepAwake::Off => "Normal auto-lock".to_string(),
                    },
                    Some((
                        "settings_keep_awake",
                        "Change",
                        false,
                        Box::new(crate::keep_awake::cycle_mode),
                    )),
                ))
                .child(
                    text(move || format!("Now: {}", crate::keep_awake::mode().get().label()))
                        .test_id("settings_keep_awake_value")
                        .px(px(14.0))
                        .text_size(px(14.0))
                        .text_color(rgba(GREEN)),
                )
                .child(settings_action(
                    "settings_debug_row",
                    "Debug log",
                    || "Connection and sign-in details".to_string(),
                    Some((
                        "settings_debug",
                        "Open",
                        false,
                        Box::new(move || {
                            settings_open.set(false);
                            debug_open.set(true);
                        }),
                    )),
                ))
                .child(view().h(px(1.0)).mx(px(14.0)).my(px(4.0)).bg(rgba(HAIRLINE)))
                .child(dynamic(move || match account.get() {
                    1 => settings_action(
                        "settings_spotify",
                        "Spotify",
                        device_note,
                        Some((
                            "settings_sign_out",
                            "Sign out",
                            true,
                            Box::new(move || {
                                settings_open.set(false);
                                spotify::send(spotify_state, SpotifyCommand::SignOut);
                            }),
                        )),
                    )
                    .into_element(),
                    2 => settings_action("settings_spotify", "Spotify", device_note, None).into_element(),
                    _ => settings_action(
                        "settings_spotify",
                        "Spotify",
                        device_note,
                        Some((
                            "settings_sign_in",
                            "Sign in",
                            false,
                            Box::new(move || {
                                settings_open.set(false);
                                spotify::send(spotify_state, SpotifyCommand::SignIn);
                            }),
                        )),
                    )
                    .into_element(),
                })),
        )
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
        // Fixed width, never squeezed: the Music card is the one that gives way.
        .style_when(is_landscape, |s| s.w(px(PANEL_W_PT)).flex_shrink_0())
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

/// The song the add sheet was opened for. Copied when the plus is tapped: if the track changes
/// while the sheet is open, saving still refers to this one.
#[derive(Clone, Debug, Default, PartialEq)]
struct AddTarget {
    track_uri: String,
    title: String,
}

/// What the Music card needs from outside itself, so the tests can swap in a fake backend.
#[derive(Clone)]
struct MusicHooks {
    /// `Some` while the add-to-playlist sheet is open.
    target: Signal<Option<AddTarget>>,
    save: Signal<SaveTrackState>,
    inspect: Rc<dyn Fn(&str)>,
    save_to: Rc<dyn Fn(&str, Vec<String>)>,
    /// Sets the volume of the Spotify device that is playing (never this iPad), 0..=100.
    set_volume: Rc<dyn Fn(u8)>,
}

impl MusicHooks {
    fn live(spotify_state: Signal<SpotifyState>) -> Self {
        Self {
            target: signal(None),
            save: spotify::save_state(),
            inspect: Rc::new(|uri: &str| spotify::inspect_saved_track(uri)),
            save_to: Rc::new(|uri: &str, to: Vec<String>| spotify::save_track_to(uri, to)),
            set_volume: Rc::new(move |v: u8| spotify::send(spotify_state, SpotifyCommand::SetVolume(v))),
        }
    }
}

fn is_track_uri(uri: &str) -> bool {
    uri.strip_prefix("spotify:track:").is_some_and(|id| !id.is_empty())
}

fn music_card(
    spotify_state: Signal<SpotifyState>,
    playlists: Signal<Vec<PlaylistSummary>>,
    hooks: MusicHooks,
) -> impl IntoElement {
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

    // The card takes whatever room the row gives it (more or less, with the timer open or
    // not) and never grows past it: `min_w(0)` lets it shrink below its content, and its
    // contents (`connected_view`) measure the room and size themselves to fit. It clips as a
    // last resort so nothing can ever draw over the timer panel.
    view()
        .test_id("music_status")
        .gap(px(8.0))
        .p(px(20.0))
        .rounded(px(28.0))
        .border_1()
        .border_color(rgba(HAIRLINE))
        .bg(rgba(CARD))
        .overflow_hidden()
        .style_when(is_landscape, |s| s.flex_1().min_w(px(0.0)).min_h(px(0.0)))
        .child(
            text("Music")
                .role(AccessibilityRole::Header)
                .text_size(px(16.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgba(GREEN)),
        )
        .child(
            view()
                .flex_1()
                .min_h(px(0.0))
                .child(dynamic(move || match kind.get() {
                    0 => unavailable_view(message).into_element(),
                    1 => signed_out_view(spotify_state).into_element(),
                    2 => awaiting_view(spotify_state, approval).into_element(),
                    3 => connecting_view().into_element(),
                    4 => connected_view(spotify_state, playback, playlists, hooks.clone()).into_element(),
                    _ => error_view(spotify_state, message).into_element(),
                })),
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
        .child(
            view()
                .flex_row()
                .gap(px(14.0))
                .child(
                    view()
                        .role(AccessibilityRole::Button)
                        .test_id("spotify_open_pair")
                        .label("Open in Safari")
                        .hint("Opens the Spotify pairing page with the code filled in")
                        .on_press(move || {
                            let code = approval.get_untracked().0;
                            let url = format!("https://spotify.com/pair?code={code}");
                            crate::debug_log::info(format!("sign-in: opening {url} in Safari"));
                            // Pairing keeps polling while Safari is in front; come back after approving.
                            let _ = open_url(url);
                        })
                        .min_h(px(60.0))
                        .min_w(px(120.0))
                        .px(px(28.0))
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(rgba(GREEN))
                        .active(|s| s.opacity(0.75))
                        .child(
                            text("Open in Safari")
                                .text_size(px(22.0))
                                .font_weight(FontWeight::BOLD)
                                .text_color(rgba(ON_GREEN)),
                        ),
                )
                .child(music_button(
                    spotify_state,
                    SpotifyCommand::SignOut,
                    "spotify_cancel",
                    "Cancel",
                    false,
                )),
        )
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

/// What the connected card needs to size itself: its own width and height (set by `on_layout`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Room {
    w: f32,
    h: f32,
}

impl Room {
    /// Beside the timer panel (or in a small window) the card is narrow: smaller type and art.
    fn narrow(self) -> bool {
        self.w > 0.0 && self.w < 720.0
    }

    /// Side of the album art: as big as the room allows, never wider than 42% of it, and
    /// leaving the playlist row its space.
    fn art_side(self, landscape: bool, playlists_h: f32) -> f32 {
        if self.w <= 0.0 || self.h <= 0.0 {
            return if landscape { 220.0 } else { 280.0 };
        }
        if landscape {
            (self.h - playlists_h - 8.0).min(self.w * 0.42).clamp(120.0, 380.0)
        } else {
            self.w.min(320.0).clamp(160.0, 320.0)
        }
    }
}

/// Height of the playlist row (covers) plus the gap above it.
const PLAYLIST_COVER_PT: f32 = 72.0;
const PLAYLIST_ROW_PT: f32 = PLAYLIST_COVER_PT + 14.0;

fn device_line(p: &Playback) -> String {
    match (&p.device, p.is_playing) {
        (Some(d), true) => format!("Playing on {d}"),
        (Some(d), false) => format!("Paused on {d}"),
        (None, _) => "Paused".to_string(),
    }
}

fn connected_view(
    spotify_state: Signal<SpotifyState>,
    playback: Memo<Playback>,
    playlists: Signal<Vec<PlaylistSummary>>,
    hooks: MusicHooks,
) -> impl IntoElement {
    let has_track = computed(move || playback.get().title.is_some());
    let artwork = computed(move || playback.get().artwork_url);
    let has_playlists = computed(move || !playlists.get().is_empty());
    // How much room this view has: the art, type and controls follow it.
    let room = signal(Room::default());
    let landscape_now = || is_landscape();

    view()
        .test_id("spotify_connected")
        .flex_1()
        .min_h(px(0.0))
        .gap(px(14.0))
        .on_layout(move |b: &Bounds<Points>| {
            let next = Room { w: b.size.width.0, h: b.size.height.0 };
            if (next.w - room.get_untracked().w).abs() > 0.5 || (next.h - room.get_untracked().h).abs() > 0.5 {
                room.set(next);
            }
        })
        .child(
            when(move || has_track.get(), move || {
                let narrow = move || room.get().narrow();
                let side = move || {
                    let pl_h = if has_playlists.get() { PLAYLIST_ROW_PT } else { 0.0 };
                    px(room.get().art_side(landscape_now(), pl_h))
                };
                view()
                    .test_id("spotify_track")
                    .flex_1()
                    .min_h(px(0.0))
                    .gap(px(22.0))
                    .items_center()
                    .style_when(is_landscape, |s| s.flex_row())
                    .child(album_art(artwork, side))
                    .child(
                        view()
                            .gap(px(8.0))
                            .min_w(px(0.0))
                            .justify_center()
                            .style_when(is_landscape, |s| s.flex_1())
                            .style_when(|| !is_landscape(), |s| s.w_full())
                            .child(
                                text(move || device_line(&playback.get()))
                                    .test_id("spotify_device")
                                    .text_size(px(16.0))
                                    .number_of_lines(1)
                                    .text_color(rgba(GREEN)),
                            )
                            .child(
                                // Title with the "add to a playlist" plus beside it.
                                view()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(12.0))
                                    .child(
                                        view().flex_1().min_w(px(0.0)).child(
                                            text(move || playback.get().title.unwrap_or_default())
                                                .test_id("spotify_title")
                                                .text_size(move || if narrow() { px(28.0) } else { px(40.0) })
                                                .line_height(move || if narrow() { px(33.0) } else { px(46.0) })
                                                .font_weight(FontWeight::BOLD)
                                                .number_of_lines(2)
                                                .text_color(white()),
                                        ),
                                    )
                                    .child(add_button(playback, hooks.clone())),
                            )
                            .child(
                                text(move || playback.get().artists.unwrap_or_default())
                                    .test_id("spotify_artists")
                                    .text_size(move || if narrow() { px(20.0) } else { px(24.0) })
                                    .number_of_lines(1)
                                    .text_color(rgba(0xffffffd0)),
                            )
                            .child(
                                text(move || playback.get().album.unwrap_or_default())
                                    .test_id("spotify_album")
                                    .text_size(px(17.0))
                                    .number_of_lines(1)
                                    .text_color(rgba(FAINT)),
                            )
                            .child(progress_block(spotify_state, playback))
                            .child(transport_row(spotify_state, playback))
                            .child(volume_row(playback, hooks.clone())),
                    )
            })
            .otherwise(move || {
                state_column("spotify_idle")
                    .min_h(px(0.0))
                    .child(state_title("Nothing playing"))
                    .child(state_note(move || match playback.get().device {
                        Some(d) => format!("{d} is ready. Start a song in Spotify."),
                        None => "Start a song in Spotify on any device.".to_string(),
                    }))
                    .child(transport_row(spotify_state, playback))
            }),
        )
        .child(when(move || has_playlists.get(), move || playlist_row(playback, playlists)))
}

fn album_art(artwork: Memo<Option<String>>, side: impl Fn() -> Points + Copy + 'static) -> impl IntoElement {
    when(move || artwork.get().is_some(), move || {
        image(move || artwork.get().unwrap_or_default())
            .test_id("spotify_art")
            .label("Album art")
            .w(side)
            .h(side)
            .flex_shrink_0()
            .rounded(px(22.0))
            .bg(rgba(0xffffff0f))
    })
    .otherwise(move || {
        view()
            .test_id("spotify_art_placeholder")
            .w(side)
            .h(side)
            .flex_shrink_0()
            .rounded(px(22.0))
            .bg(rgba(0xffffff0f))
            .border_1()
            .border_color(rgba(HAIRLINE))
            .items_center()
            .justify_center()
            .child(icon(|| Icon::Note, 56.0, || 0xffffff40))
    })
}

/// The seek bar: drag (or tap) anywhere on it to jump to that point of the track. While the
/// finger is down the left time shows where it would land; letting go sends the seek.
fn progress_block(spotify_state: Signal<SpotifyState>, playback: Memo<Playback>) -> impl IntoElement {
    const HIT_H: f32 = 36.0;
    const TRACK_H: f32 = 6.0;
    const THUMB: f32 = 16.0;
    let can_control = computed(move || playback.get().can_control);
    let width = signal(0.0_f32);
    // Fraction under the finger while it is down.
    let scrub = signal(None::<f32>);
    // After letting go, the target (ms) shown until the track really gets there, so the bar
    // does not jump back while Spotify catches up. Cleared by time or by arriving.
    let landed = signal(None::<(u64, f64)>);
    let start_x = stored(0.0_f32);
    let frac_at = move |x: f32| (x / width.get_untracked().max(1.0)).clamp(0.0, 1.0);

    effect(move || {
        let p = playback.get();
        if let Some((target, deadline)) = landed.get_untracked() {
            if p.progress_ms.abs_diff(target) < 3_000 || unix_ms() > deadline {
                landed.set(None);
            }
        }
    });

    let shown_ms = move || {
        let p = playback.get();
        match (scrub.get(), landed.get()) {
            (Some(f), _) => (f * p.duration_ms as f32) as u64,
            (None, Some((target, _))) => target,
            _ => p.progress_ms,
        }
    };
    let shown_frac = move || {
        let p = playback.get();
        match (scrub.get(), landed.get()) {
            (Some(f), _) => f,
            (None, Some((target, _))) => progress_fraction(target, p.duration_ms),
            _ => progress_fraction(p.progress_ms, p.duration_ms),
        }
    };

    view()
        .gap(px(2.0))
        .child(
            view()
                .test_id("spotify_seek")
                .role(AccessibilityRole::Adjustable)
                .label("Track position")
                .accessibility_value(move || {
                    Some(nsh_core::AccessibilityValue {
                        text: Some(format!("{} of {}", format_track_time(shown_ms()), format_track_time(playback.get().duration_ms)).into()),
                        ..nsh_core::AccessibilityValue::default()
                    })
                })
                .h(px(HIT_H))
                .justify_center()
                .on_layout(move |b: &Bounds<Points>| width.set(b.size.width.0))
                // Takes the finger as soon as it lands, so a tap seeks and a drag scrubs; a page
                // scroll around it (portrait) cannot steal it.
                .claim_touches(TouchClaim::on_start())
                .keep_touches(true)
                .on_responder_grant(move |e: &ResponderEvent| {
                    if !can_control.get_untracked() || playback.get_untracked().duration_ms == 0 {
                        return;
                    }
                    start_x.replace(e.location.x.0);
                    scrub.set(Some(frac_at(e.location.x.0)));
                })
                .on_responder_move(move |e: &ResponderEvent| {
                    if scrub.get_untracked().is_some() {
                        scrub.set(Some(frac_at(start_x.get() + e.gesture.delta.x.0)));
                    }
                })
                .on_responder_release(move |_: &ResponderEvent| {
                    if let Some(f) = scrub.get_untracked() {
                        scrub.set(None);
                        let target = (f * playback.get_untracked().duration_ms as f32) as u64;
                        landed.set(Some((target, unix_ms() + 4_000.0)));
                        spotify::send(spotify_state, SpotifyCommand::SeekTo(target));
                    }
                })
                .on_responder_terminate(move |_: &ResponderEvent| scrub.set(None))
                .child(
                    view()
                        .test_id("spotify_progress")
                        .h(px(TRACK_H))
                        .rounded_full()
                        .overflow_hidden()
                        .bg(rgba(0xffffff22))
                        .child(
                            view()
                                .h(px(TRACK_H))
                                .rounded_full()
                                .bg(rgba(GREEN))
                                .w(move || relative(shown_frac())),
                        ),
                )
                // The handle shows while the bar is held.
                .child(
                    view()
                        .absolute()
                        .top(px((HIT_H - THUMB) / 2.0))
                        .left(move || relative(shown_frac()))
                        .ml(px(-THUMB / 2.0))
                        .size(px(THUMB))
                        .rounded_full()
                        .bg(white())
                        .opacity(move || if scrub.get().is_some() { 1.0_f32 } else { 0.0_f32 })
                        .pointer_events(PointerEvents::None)
                        .accessibility_elements_hidden(true),
                ),
        )
        .child(
            view()
                .flex_row()
                .justify_between()
                .child(
                    text(move || format_track_time(shown_ms()))
                        .test_id("spotify_elapsed")
                        .text_size(px(15.0))
                        .tabular_nums()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(move || if scrub.get().is_some() { rgba(GREEN) } else { rgba(MUTED) }),
                )
                .child(
                    text(move || format_track_time(playback.get().duration_ms))
                        .text_size(px(15.0))
                        .tabular_nums()
                        .text_color(rgba(MUTED)),
                ),
        )
}

/// Shuffle: 0 off, 1 shuffle, 2 Smart Shuffle.
fn shuffle_state(p: &Playback) -> u8 {
    if p.smart_shuffle {
        2
    } else if p.shuffle {
        1
    } else {
        0
    }
}

fn transport_row(spotify_state: Signal<SpotifyState>, playback: Memo<Playback>) -> impl IntoElement {
    let can_control = computed(move || playback.get().can_control);
    let off = 0xffffff99_u32;
    let on = GREEN;
    // Five icon buttons that always fit together: 48 + 52 + 68 + 52 + 48 plus four gaps.
    view()
        .test_id("spotify_transport")
        .flex_row()
        .items_center()
        .justify_center()
        .gap(px(10.0))
        .pt(px(4.0))
        .child(icon_button(
            spotify_state,
            can_control,
            SpotifyCommand::ToggleShuffle,
            "spotify_shuffle",
            move || {
                match shuffle_state(&playback.get()) {
                    2 => "Smart Shuffle on",
                    1 => "Shuffle on",
                    _ => "Shuffle off",
                }
                .to_string()
            },
            move || if shuffle_state(&playback.get()) == 2 { Icon::SmartShuffle } else { Icon::Shuffle },
            move || if shuffle_state(&playback.get()) == 0 { off } else { on },
            48.0,
            26.0,
            false,
        ))
        .child(icon_button(
            spotify_state,
            can_control,
            SpotifyCommand::Previous,
            "spotify_prev",
            || "Previous".to_string(),
            || Icon::Previous,
            || 0xffffffff,
            52.0,
            28.0,
            false,
        ))
        .child(icon_button(
            spotify_state,
            can_control,
            SpotifyCommand::PlayPause,
            "spotify_play_pause",
            move || if playback.get().is_playing { "Pause" } else { "Play" }.to_string(),
            move || if playback.get().is_playing { Icon::Pause } else { Icon::Play },
            || ON_GREEN,
            68.0,
            34.0,
            true,
        ))
        .child(icon_button(
            spotify_state,
            can_control,
            SpotifyCommand::Next,
            "spotify_next",
            || "Next".to_string(),
            || Icon::Next,
            || 0xffffffff,
            52.0,
            28.0,
            false,
        ))
        .child(icon_button(
            spotify_state,
            can_control,
            SpotifyCommand::CycleRepeat,
            "spotify_repeat",
            move || {
                match playback.get().repeat {
                    RepeatMode::Off => "Repeat off",
                    RepeatMode::Context => "Repeat all",
                    RepeatMode::Track => "Repeat one",
                }
                .to_string()
            },
            move || if playback.get().repeat == RepeatMode::Track { Icon::RepeatOne } else { Icon::Repeat },
            move || if playback.get().repeat == RepeatMode::Off { off } else { on },
            48.0,
            26.0,
            false,
        ))
}

/// A round icon-only button. `filled` = the green primary button (play/pause).
#[allow(clippy::too_many_arguments)]
fn icon_button(
    spotify_state: Signal<SpotifyState>,
    can_control: Memo<bool>,
    command: SpotifyCommand,
    test_id: &'static str,
    label: impl Fn() -> String + 'static,
    kind: impl Fn() -> Icon + 'static,
    color: impl Fn() -> u32 + 'static,
    side: f32,
    icon_size: f32,
    filled: bool,
) -> impl IntoElement {
    view()
        .role(AccessibilityRole::Button)
        .test_id(test_id)
        .label(label)
        .disabled(move || !can_control.get())
        .opacity(move || if can_control.get() { 1.0_f32 } else { 0.4_f32 })
        .on_press(move || spotify::send(spotify_state, command))
        .w(px(side))
        .h(px(side))
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(if filled { rgba(GREEN) } else { rgba(0x00000000) })
        .active(|s| s.bg(rgba(0xffffff1f)))
        .child(icon(kind, icon_size, color))
}

/// A row of square playlist covers for quick play; scrolls sideways through all of them.
fn playlist_row(playback: Memo<Playback>, playlists: Signal<Vec<PlaylistSummary>>) -> impl IntoElement {
    scroll()
        .test_id("spotify_playlists")
        .horizontal()
        .scrollbar(false)
        .h(px(PLAYLIST_COVER_PT + 8.0))
        .flex_shrink_0()
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(12.0))
                .py(px(4.0))
                .child(each(playlists, |p: &PlaylistSummary| p.uri.clone(), move |p, _| {
                    playlist_cover(playback, p)
                })),
        )
}

fn playlist_cover(playback: Memo<Playback>, item: ReadSignal<PlaylistSummary>) -> impl IntoElement {
    let name = move || item.get().name;
    let playing = move || {
        let p = playback.get();
        p.context_uri.is_some() && p.context_uri == Some(item.get().uri)
    };
    view()
        .role(AccessibilityRole::Button)
        .test_id("spotify_playlist")
        .label(name)
        .hint("Plays this playlist")
        .on_press(move || spotify::play_context(&item.get_untracked().uri))
        .size(px(PLAYLIST_COVER_PT))
        .flex_shrink_0()
        .rounded(px(14.0))
        .overflow_hidden()
        .border_2()
        .border_color(move || if playing() { rgba(GREEN) } else { rgba(0x00000000) })
        .bg(rgba(0xffffff12))
        .items_center()
        .justify_center()
        .active(|s| s.opacity(0.7))
        .child(playlist_tile(item, PLAYLIST_COVER_PT - 4.0, false))
}

// ---------------------------------------------------------------------------------------------
// Playlist tiles: the cover when Spotify has one, otherwise something readable (never a note).
// ---------------------------------------------------------------------------------------------

/// Background pairs (from, to) for tiles that only have a name. Dark enough for white text.
const TILE_COLORS: [(u32, u32); 6] = [
    (0x3b5bdbff, 0x233a99ff),
    (0x7048e8ff, 0x4527a0ff),
    (0xc2255cff, 0x7d1238ff),
    (0xd9480fff, 0x8f2d06ff),
    (0x2b8a3eff, 0x14532aff),
    (0x0c8599ff, 0x08525fff),
];

/// A steady colour per playlist, so the same playlist always looks the same.
fn tile_colors(seed: &str) -> (u32, u32) {
    let mut h: u32 = 0x811c9dc5;
    for b in seed.bytes() {
        h = (h ^ b as u32).wrapping_mul(0x01000193);
    }
    TILE_COLORS[(h as usize) % TILE_COLORS.len()]
}

/// Up to three capital letters for a long name: "Chill Lofi Study Beats" is "CLS"; one long
/// word gives its first three letters.
fn initials(name: &str) -> String {
    let words: Vec<&str> = name.split_whitespace().collect();
    let letters = |w: &str| w.chars().filter(|c| c.is_alphanumeric()).collect::<Vec<char>>();
    let mut out: String = if words.len() <= 1 {
        words.first().map(|w| letters(w)).unwrap_or_default().into_iter().take(3).flat_map(char::to_uppercase).collect()
    } else {
        words
            .iter()
            .filter_map(|w| letters(w).first().copied())
            .take(3)
            .flat_map(char::to_uppercase)
            .collect()
    };
    if out.is_empty() {
        out = name.trim().chars().take(2).collect();
    }
    if out.is_empty() {
        out = "?".to_string();
    }
    out
}

/// The text for a playlist that has no cover: the name itself when it fits two short lines
/// (`false`), otherwise its initials (`true`).
fn short_label(name: &str) -> (String, bool) {
    const LINE: usize = 9;
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return ("?".to_string(), true);
    }
    let (mut lines, mut used) = (1, 0);
    for word in name.split(' ') {
        let n = word.chars().count();
        if n > LINE {
            return (initials(&name), true);
        }
        if used == 0 {
            used = n;
        } else if used + 1 + n <= LINE {
            used += 1 + n;
        } else {
            lines += 1;
            used = n;
        }
    }
    if lines <= 2 { (name, false) } else { (initials(&name), true) }
}

/// What a playlist looks like inside a `side` x `side` square. Liked Songs is always a heart,
/// Spotify DJ always its blue and green tile, a playlist with a cover shows it, and one without
/// shows its short name. `compact` (small tiles that sit beside the full name) uses initials.
fn playlist_tile(item: ReadSignal<PlaylistSummary>, side: f32, compact: bool) -> impl IntoElement {
    dynamic(move || {
        let p = item.get();
        if p.is_liked_songs() {
            liked_tile(side).into_element()
        } else if p.is_dj() {
            dj_tile(side, p.image_url).into_element()
        } else if let Some(url) = p.image_url.clone() {
            image(url)
                .size(px(side))
                .flex_shrink_0()
                .accessibility_elements_hidden(true)
                .into_element()
        } else {
            name_tile(&p, side, compact).into_element()
        }
    })
}

fn liked_tile(side: f32) -> View {
    view()
        .test_id("playlist_tile_liked")
        .size(px(side))
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .bg_linear(deg(135.0))
        .gradient_from(rgba(0x450af5ff))
        .gradient_to(rgba(0x8e8ee5ff))
        .accessibility_elements_hidden(true)
        .child(icon(|| Icon::Heart, side * 0.44, || 0xffffffff))
}

/// Spotify's DJ cover: a blue square with a green circular glyph and a small "DJ". An official
/// cover from Spotify (`image_url`) is drawn on top of it; the tile shows if that never loads.
fn dj_tile(side: f32, cover: Option<String>) -> View {
    view()
        .test_id("playlist_tile_dj")
        .size(px(side))
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .bg_linear(deg(160.0))
        .gradient_from(rgba(0x2d5bffff))
        .gradient_to(rgba(0x0a2bb8ff))
        .accessibility_elements_hidden(true)
        .child(icon(|| Icon::Dj, side * 0.74, || GREEN))
        .child(
            text("DJ")
                .absolute()
                .left(px(side * 0.09))
                .bottom(px(side * 0.06))
                .text_size(px((side * 0.16).max(9.0)))
                .font_weight(FontWeight::BOLD)
                .text_color(rgba(GREEN)),
        )
        .child(when(cover.is_some(), move || {
            image(cover.clone().unwrap_or_default())
                .absolute()
                .top(px(0.0))
                .left(px(0.0))
                .size(px(side))
        }))
}

fn name_tile(p: &PlaylistSummary, side: f32, compact: bool) -> View {
    let (label, is_initials) = if compact { (initials(&p.name), true) } else { short_label(&p.name) };
    let (from, to) = tile_colors(&p.uri);
    let size = if is_initials { side * 0.34 } else { (side * 0.19).max(11.0) };
    view()
        .test_id("playlist_tile_name")
        .size(px(side))
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .px(px(side * 0.07))
        .bg_linear(deg(135.0))
        .gradient_from(rgba(from))
        .gradient_to(rgba(to))
        .accessibility_elements_hidden(true)
        .child(
            text(label)
                .text_center()
                .number_of_lines(2)
                .text_size(px(size))
                .line_height(px(size * 1.15))
                .font_weight(FontWeight::BOLD)
                .text_color(white()),
        )
}

// ---------------------------------------------------------------------------------------------
// Volume of the Spotify device that is playing (not this iPad's speaker).
// ---------------------------------------------------------------------------------------------

fn volume_icon(percent: u8) -> Icon {
    match percent {
        0 => Icon::VolumeOff,
        1..=49 => Icon::VolumeLow,
        _ => Icon::VolumeHigh,
    }
}

/// Volume in percent for a touch `x` points along a bar `width` wide.
fn volume_at(x: f32, width: f32) -> u8 {
    ((x / width.max(1.0)).clamp(0.0, 1.0) * 100.0).round() as u8
}

/// A slider for the active Spotify device's volume. Dragging only previews; the volume is sent
/// once, when the finger lifts (a tap works too). Hidden when the device cannot change volume.
fn volume_row(playback: Memo<Playback>, hooks: MusicHooks) -> impl IntoElement {
    const HIT_H: f32 = 36.0;
    const TRACK_H: f32 = 6.0;
    const THUMB: f32 = 18.0;
    let supported = computed(move || playback.get().can_set_volume);
    let set_volume = hooks.set_volume;
    when(move || supported.get(), move || {
        let set_volume = set_volume.clone();
        let width = signal(0.0_f32);
        // The value under the finger while it is down.
        let scrub = signal(None::<u8>);
        // After letting go: the value shown until the device reports it (or time runs out).
        let landed = signal(None::<(u8, f64)>);
        let start_x = stored(0.0_f32);
        effect(move || {
            let p = playback.get();
            if let Some((target, deadline)) = landed.get_untracked() {
                if p.volume_percent.is_some_and(|v| v.abs_diff(target) <= 1) || unix_ms() > deadline {
                    landed.set(None);
                }
            }
        });
        let shown = move || {
            scrub
                .get()
                .or_else(|| landed.get().map(|(v, _)| v))
                .or_else(|| playback.get().volume_percent)
        };
        let frac = move || shown().map_or(0.0, |v| v as f32 / 100.0);
        let send = {
            let set_volume = set_volume.clone();
            move |v: u8| {
                let v = v.min(100);
                landed.set(Some((v, unix_ms() + 4_000.0)));
                set_volume(v);
            }
        };
        let send_for_release = send.clone();
        let step = move |delta: i32| {
            let now = shown().unwrap_or(0) as i32;
            send((now + delta).clamp(0, 100) as u8);
        };
        view()
            .test_id("spotify_volume_row")
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .child(icon(
                move || volume_icon(shown().unwrap_or(0)),
                22.0,
                move || if scrub.get().is_some() { GREEN } else { 0xffffffb3 },
            ))
            .child(
                view()
                    .test_id("spotify_volume")
                    .role(AccessibilityRole::Adjustable)
                    .label(move || match playback.get().device {
                        Some(d) => format!("Volume on {d}"),
                        None => "Volume of the playing device".to_string(),
                    })
                    .hint("Changes the volume of the device that is playing, not this iPad")
                    .accessibility_value(move || {
                        Some(nsh_core::AccessibilityValue {
                            text: Some(
                                shown().map_or("Unknown".to_string(), |v| format!("{v} percent")).into(),
                            ),
                            ..nsh_core::AccessibilityValue::default()
                        })
                    })
                    .accessibility_actions(vec![
                        nsh_core::AccessibilityActionDef { name: "increment".into(), label: None },
                        nsh_core::AccessibilityActionDef { name: "decrement".into(), label: None },
                    ])
                    .on_accessibility_action(move |action: &str| match action {
                        "increment" => step(5),
                        "decrement" => step(-5),
                        _ => {}
                    })
                    .flex_1()
                    .min_w(px(0.0))
                    .h(px(HIT_H))
                    .justify_center()
                    .on_layout(move |b: &Bounds<Points>| width.set(b.size.width.0))
                    .claim_touches(TouchClaim::on_start())
                    .keep_touches(true)
                    .on_responder_grant(move |e: &ResponderEvent| {
                        start_x.replace(e.location.x.0);
                        scrub.set(Some(volume_at(e.location.x.0, width.get_untracked())));
                    })
                    .on_responder_move(move |e: &ResponderEvent| {
                        if scrub.get_untracked().is_some() {
                            scrub.set(Some(volume_at(start_x.get() + e.gesture.delta.x.0, width.get_untracked())));
                        }
                    })
                    .on_responder_release(move |_: &ResponderEvent| {
                        if let Some(v) = scrub.get_untracked() {
                            scrub.set(None);
                            send_for_release(v);
                        }
                    })
                    .on_responder_terminate(move |_: &ResponderEvent| scrub.set(None))
                    .child(
                        view()
                            .h(px(TRACK_H))
                            .rounded_full()
                            .overflow_hidden()
                            .bg(rgba(0xffffff22))
                            .child(
                                view()
                                    .h(px(TRACK_H))
                                    .rounded_full()
                                    .bg(move || if scrub.get().is_some() { rgba(GREEN) } else { rgba(0xffffffd9) })
                                    .w(move || relative(frac())),
                            ),
                    )
                    .child(
                        view()
                            .absolute()
                            .top(px((HIT_H - THUMB) / 2.0))
                            .left(move || relative(frac()))
                            .ml(px(-THUMB / 2.0))
                            .size(px(THUMB))
                            .rounded_full()
                            .bg(white())
                            .opacity(move || if scrub.get().is_some() { 1.0_f32 } else { 0.0_f32 })
                            .pointer_events(PointerEvents::None)
                            .accessibility_elements_hidden(true),
                    ),
            )
            .child(
                view().w(px(44.0)).items_end().child(
                    text(move || shown().map_or("--".to_string(), |v| format!("{v}%")))
                        .test_id("spotify_volume_value")
                        .text_size(px(15.0))
                        .tabular_nums()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(move || if scrub.get().is_some() { rgba(GREEN) } else { rgba(MUTED) }),
                ),
            )
    })
}

// ---------------------------------------------------------------------------------------------
// Add to playlist: a plus beside the title opens a sheet for the song that is playing.
// ---------------------------------------------------------------------------------------------

/// The round plus next to the title. Off when there is no Spotify track (an ad, a podcast or
/// nothing loaded).
fn add_button(playback: Memo<Playback>, hooks: MusicHooks) -> impl IntoElement {
    let ready = computed(move || playback.get().track_uri.as_deref().is_some_and(is_track_uri));
    let target = hooks.target;
    let inspect = hooks.inspect;
    view()
        .role(AccessibilityRole::Button)
        .test_id("spotify_add")
        .label("Add to playlist or Liked Songs")
        .hint("Opens a list of your playlists to add this song to")
        .disabled(move || !ready.get())
        .opacity(move || if ready.get() { 1.0_f32 } else { 0.35_f32 })
        .on_press(move || {
            let p = playback.get_untracked();
            let Some(uri) = p.track_uri.filter(|u| is_track_uri(u)) else { return };
            // The sheet keeps this song even if the track changes while it is open.
            target.set(Some(AddTarget { track_uri: uri.clone(), title: p.title.unwrap_or_default() }));
            inspect(&uri);
        })
        .size(px(46.0))
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_2()
        .border_color(rgba(0xffffff4d))
        .active(|s| s.bg(rgba(0xffffff1f)))
        .child(icon(|| Icon::Plus, 24.0, || 0xffffffff))
}

/// Playlists a song can be added to: editable ones and Liked Songs, never Spotify DJ.
fn add_targets(all: &[PlaylistSummary], query: &str) -> Vec<PlaylistSummary> {
    let q = query.trim().to_lowercase();
    all.iter()
        .filter(|p| p.can_add_tracks && !p.is_dj())
        .filter(|p| q.is_empty() || p.name.to_lowercase().contains(&q))
        .cloned()
        .collect()
}

/// `"1 song"`, `"12 songs"`, or `""` when Spotify did not say.
fn song_count_text(count: Option<u32>) -> String {
    match count {
        Some(1) => "1 song".to_string(),
        Some(n) => format!("{n} songs"),
        None => String::new(),
    }
}

fn sheet_button(
    test_id: &'static str,
    label: impl Fn() -> String + 'static,
    primary: bool,
    disabled: impl Fn() -> bool + Copy + 'static,
    on_press: impl Fn() + 'static,
) -> impl IntoElement {
    let label = Rc::new(label);
    let label_for_text = label.clone();
    view()
        .role(AccessibilityRole::Button)
        .test_id(test_id)
        .label(move || label())
        .disabled(disabled)
        .opacity(move || if disabled() { 0.5_f32 } else { 1.0_f32 })
        .on_press(move || on_press())
        .min_h(px(52.0))
        .min_w(px(110.0))
        .px(px(26.0))
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .border_color(if primary { rgba(GREEN) } else { rgba(0xffffff33) })
        .bg(if primary { rgba(GREEN) } else { rgba(0xffffff0f) })
        .active(|s| s.opacity(0.75))
        .child(
            text(move || label_for_text())
                .text_size(px(19.0))
                .font_weight(FontWeight::BOLD)
                .text_color(if primary { rgba(ON_GREEN) } else { rgba(0xffffffff) }),
        )
}

/// One playlist in the sheet: cover, name, and a circle on the right. A playlist that already
/// has the song shows "Saved" and cannot be picked again.
fn sheet_row(
    item: ReadSignal<PlaylistSummary>,
    saved: Memo<Vec<String>>,
    selected: Signal<Vec<String>>,
    locked: impl Fn() -> bool + Copy + 'static,
) -> impl IntoElement {
    let is_saved = move || saved.get().contains(&item.get().uri);
    let is_picked = move || selected.get().contains(&item.get().uri);
    let checked = move || is_saved() || is_picked();
    view()
        .role(AccessibilityRole::Checkbox)
        .test_id("add_sheet_row")
        .label(move || item.get().name)
        .hint(move || if is_saved() { "Already added".to_string() } else { "Tap to choose".to_string() })
        .accessibility_state(move || AccessibilityState {
            checked: Some(if checked() { Checked::True } else { Checked::False }),
            ..AccessibilityState::default()
        })
        // A song that is already there cannot be picked again; while busy nothing can change.
        .disabled(move || is_saved() || locked())
        .on_press(move || {
            if is_saved() || locked() {
                return;
            }
            let uri = item.get_untracked().uri;
            selected.update(|v| {
                if let Some(i) = v.iter().position(|u| *u == uri) {
                    v.remove(i);
                } else {
                    v.push(uri);
                }
            });
        })
        .flex_row()
        .items_center()
        .gap(px(14.0))
        .min_h(px(64.0))
        .px(px(10.0))
        .rounded(px(14.0))
        .opacity(move || if locked() && !is_saved() { 0.5_f32 } else { 1.0_f32 })
        .active(|s| s.bg(rgba(0xffffff12)))
        .child(
            view()
                .size(px(48.0))
                .flex_shrink_0()
                .rounded(px(8.0))
                .overflow_hidden()
                .child(playlist_tile(item, 48.0, true)),
        )
        .child(
            view()
                .flex_1()
                .min_w(px(0.0))
                .child(
                    text(move || item.get().name)
                        .text_size(px(19.0))
                        .font_weight(FontWeight::MEDIUM)
                        .number_of_lines(1)
                        .text_color(white()),
                )
                .child(
                    text(move || {
                        if is_saved() {
                            "Saved".to_string()
                        } else {
                            song_count_text(item.get().song_count)
                        }
                    })
                    .test_id("add_sheet_row_note")
                    .text_size(px(14.0))
                    .number_of_lines(1)
                    .text_color(move || if is_saved() { rgba(GREEN) } else { rgba(FAINT) }),
                ),
        )
        .child(
            view()
                .size(px(28.0))
                .flex_shrink_0()
                .rounded_full()
                .border_2()
                .border_color(move || if checked() { rgba(GREEN) } else { rgba(0xffffff66) })
                .bg(move || if checked() { rgba(GREEN) } else { rgba(0x00000000) })
                .items_center()
                .justify_center()
                .child(when(checked, || icon(|| Icon::Check, 16.0, || ON_GREEN))),
        )
}

/// The "Add to playlist" sheet. It works on the snapshot taken when it opened. "Done" asks the
/// backend to add the song to the newly picked places and the sheet only says it worked when
/// the backend's state says so.
fn add_sheet(
    target: Signal<Option<AddTarget>>,
    playlists: Signal<Vec<PlaylistSummary>>,
    hooks: MusicHooks,
) -> impl IntoElement {
    let snapshot = target.get_untracked().unwrap_or_default();
    let track_uri = snapshot.track_uri.clone();
    let track_title = snapshot.title.clone();
    let save = hooks.save;
    let save_to = hooks.save_to.clone();

    let query = signal(String::new());
    let selected = signal(Vec::<String>::new());
    let submitted = signal(false);
    let saw_busy = signal(false);
    let notice_before = stored(None::<String>);

    // The backend state counts only when it is about this song.
    let uri_for_status = track_uri.clone();
    let status = computed(move || {
        let s = save.get();
        (s.track_uri == uri_for_status).then_some(s)
    });
    let loading = move || status.get().is_none_or(|s| s.loading);
    let busy = move || status.get().is_some_and(|s| s.busy);
    let error = move || status.get().and_then(|s| s.error);
    let notice = move || if submitted.get() { status.get().and_then(|s| s.notice) } else { None };
    let saved = computed(move || status.get().map(|s| s.saved_in).unwrap_or_default());
    let locked = move || loading() || busy();

    let visible = computed(move || add_targets(&playlists.get(), &query.get()));
    let any_targets = computed(move || playlists.get().iter().any(|p| p.can_add_tracks && !p.is_dj()));

    // Success is when the backend finished (not busy), reports no error and has a notice, after
    // this sheet's own Done. Then the sheet closes by itself after a moment.
    effect(move || {
        if !submitted.get() {
            return;
        }
        let Some(s) = status.get() else { return };
        if s.busy {
            saw_busy.set(true);
            return;
        }
        let fresh = saw_busy.get_untracked() || s.notice != notice_before.get();
        if s.error.is_none() && s.notice.is_some() && fresh {
            let closer = spawn(async move {
                timer(Duration::from_millis(1400)).await;
                target.set(None);
            });
            cleanup(move || closer.cancel());
        }
    });

    let uri_for_done = track_uri.clone();
    let done = move || {
        let have = saved.get_untracked();
        let fresh_picks: Vec<String> =
            selected.get_untracked().into_iter().filter(|u| !have.contains(u)).collect();
        if fresh_picks.is_empty() {
            target.set(None);
            return;
        }
        if busy() {
            return;
        }
        notice_before.replace(status.get_untracked().and_then(|s| s.notice));
        saw_busy.set(false);
        submitted.set(true);
        save_to(&uri_for_done, fresh_picks);
    };

    let panel_h = move || {
        let room = window().height.0 - keyboard_height().0 - 48.0;
        px(room.clamp(300.0, 660.0))
    };

    view()
        .test_id("add_sheet")
        .absolute()
        .inset_0()
        .items_center()
        .pt(px(24.0))
        .px(px(24.0))
        .child(
            view()
                .test_id("add_sheet_backdrop")
                .role(AccessibilityRole::Button)
                .label("Cancel")
                .absolute()
                .inset_0()
                .bg(rgba(0x000000b8))
                .on_press(move || target.set(None)),
        )
        .child(
            view()
                .test_id("add_sheet_panel")
                .role(AccessibilityRole::Group)
                .label("Add to playlist")
                .w_full()
                .max_w(px(560.0))
                .h(panel_h)
                .p(px(20.0))
                .gap(px(12.0))
                .rounded(px(28.0))
                .border_1()
                .border_color(rgba(0xffffff26))
                .bg(rgba(0x1b1b1eff))
                .shadow_2xl()
                .child(
                    view()
                        .gap(px(2.0))
                        .child(
                            text("Add to playlist")
                                .test_id("add_sheet_title")
                                .role(AccessibilityRole::Header)
                                .text_size(px(26.0))
                                .font_weight(FontWeight::BOLD)
                                .text_color(white()),
                        )
                        .child(
                            text(track_title)
                                .test_id("add_sheet_track")
                                .text_size(px(16.0))
                                .number_of_lines(1)
                                .text_color(rgba(MUTED)),
                        ),
                )
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap(px(10.0))
                        .h(px(48.0))
                        .px(px(14.0))
                        .rounded(px(12.0))
                        .bg(rgba(0xffffff14))
                        .child(icon(|| Icon::Search, 20.0, || 0xffffff99))
                        .child(
                            nsh_core::element::input()
                                .test_id("add_sheet_search")
                                .label("Find a playlist")
                                .placeholder("Find a playlist")
                                .value(move || query.get())
                                .on_change(move |text: &str| query.set(text.to_owned()))
                                .auto_capitalize(AutoCapitalize::None)
                                .auto_correct(false)
                                .keyboard_appearance(KeyboardAppearance::Dark)
                                .flex_1()
                                .min_w(px(0.0))
                                .h(px(44.0))
                                .text_size(px(18.0))
                                .text_color(white()),
                        ),
                )
                .child(
                    scroll()
                        .test_id("add_sheet_list")
                        .flex_1()
                        .min_h(px(0.0))
                        .scrollbar(false)
                        .child(
                            view()
                                .gap(px(2.0))
                                .child(each(
                                    move || visible.get(),
                                    |p: &PlaylistSummary| p.uri.clone(),
                                    move |item, _| sheet_row(item, saved, selected, locked),
                                ))
                                .child(when(move || visible.get().is_empty(), move || {
                                    text(move || {
                                        if !any_targets.get() {
                                            "No playlists you can add to yet."
                                        } else {
                                            "No playlist matches that name."
                                        }
                                    })
                                    .test_id("add_sheet_empty")
                                    .px(px(10.0))
                                    .py(px(18.0))
                                    .text_size(px(17.0))
                                    .text_color(rgba(FAINT))
                                })),
                        ),
                )
                .child(
                    // One line for what is going on: checking, saving, a problem, or done.
                    text(move || {
                        if let Some(e) = error() {
                            e
                        } else if busy() {
                            "Saving\u{2026}".to_string()
                        } else if let Some(n) = notice() {
                            n
                        } else if loading() {
                            "Checking where this song is saved\u{2026}".to_string()
                        } else {
                            String::new()
                        }
                    })
                    .test_id("add_sheet_status")
                    .role(AccessibilityRole::Alert)
                    .text_size(px(15.0))
                    .line_height(px(20.0))
                    .number_of_lines(2)
                    .text_color(move || {
                        if error().is_some() {
                            rgba(0xff8d8dff)
                        } else if notice().is_some() && !busy() {
                            rgba(GREEN)
                        } else {
                            rgba(MUTED)
                        }
                    }),
                )
                .child(
                    view()
                        .flex_row()
                        .justify_end()
                        .gap(px(12.0))
                        .child(sheet_button("add_sheet_cancel", || "Cancel".to_string(), false, || false, move || {
                            target.set(None)
                        }))
                        .child(sheet_button(
                            "add_sheet_done",
                            move || if busy() { "Saving\u{2026}".to_string() } else { "Done".to_string() },
                            true,
                            busy,
                            done,
                        )),
                ),
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
        view().flex_1().p(px(24.0)).child(music_card(s, spotify::playlists(), test_hooks(s)))
    }

    // A fake backend for the add sheet and the volume slider: it records what the UI asked for.
    // The headless app may run on another thread than the test, so this is process-wide, and
    // the tests that use it take `fake_lock()` so they never overlap.
    use std::sync::Mutex;
    static VOLUMES: Mutex<Vec<u8>> = Mutex::new(Vec::new());
    static INSPECTED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static SAVES: Mutex<Vec<(String, Vec<String>)>> = Mutex::new(Vec::new());
    /// Where the fake says the song already is.
    static ALREADY: Mutex<Vec<String>> = Mutex::new(Vec::new());
    /// When set, a save fails with this message instead of succeeding.
    static FAIL_WITH: Mutex<Option<String>> = Mutex::new(None);
    /// A state the test wants the running app to switch to (picked up by `SheetDashboard`).
    static NEXT_STATE: Mutex<Option<SpotifyState>> = Mutex::new(None);
    static FAKE_LOCK: Mutex<()> = Mutex::new(());

    /// Serialises the tests that use the fake backend and clears it.
    fn fake_lock() -> std::sync::MutexGuard<'static, ()> {
        let guard = FAKE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        VOLUMES.lock().unwrap().clear();
        INSPECTED.lock().unwrap().clear();
        SAVES.lock().unwrap().clear();
        ALREADY.lock().unwrap().clear();
        *FAIL_WITH.lock().unwrap() = None;
        *NEXT_STATE.lock().unwrap() = None;
        guard
    }
    fn test_hooks(_state: Signal<SpotifyState>) -> MusicHooks {
        let save = signal(SaveTrackState::default());
        MusicHooks {
            target: signal(None),
            save,
            inspect: Rc::new(move |uri: &str| {
                INSPECTED.lock().unwrap().push(uri.to_string());
                save.set(SaveTrackState {
                    track_uri: uri.to_string(),
                    loading: false,
                    saved_in: ALREADY.lock().unwrap().clone(),
                    ..SaveTrackState::default()
                });
            }),
            save_to: Rc::new(move |uri: &str, to: Vec<String>| {
                SAVES.lock().unwrap().push((uri.to_string(), to.clone()));
                let mut s = save.get_untracked();
                s.busy = true;
                s.error = None;
                s.notice = None;
                save.set(s.clone());
                let uri = uri.to_string();
                spawn(async move {
                    timer(Duration::from_millis(150)).await;
                    let mut done = s;
                    done.busy = false;
                    done.track_uri = uri;
                    match FAIL_WITH.lock().unwrap().clone() {
                        Some(e) => done.error = Some(e),
                        None => {
                            done.saved_in.extend(to);
                            done.notice = Some("Added to your playlists".to_string());
                        }
                    }
                    save.set(done);
                });
            }),
            set_volume: Rc::new(|v: u8| VOLUMES.lock().unwrap().push(v)),
        }
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
            ..Playback::default()
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

    fn controllable() -> Playback {
        Playback {
            device: Some("Windows PC".into()),
            title: Some("Test Song".into()),
            artists: Some("Test Artist".into()),
            album: Some("Test Album".into()),
            is_playing: true,
            progress_ms: 0,
            duration_ms: 200_000,
            can_control: true,
            ..Playback::default()
        }
    }

    #[component]
    fn ProbeShuffleOn() -> impl IntoElement {
        probe(SpotifyState::Connected(Playback {
            shuffle: true,
            repeat: crate::spotify::RepeatMode::Context,
            ..controllable()
        }))
    }

    #[component]
    fn ProbeSmartShuffle() -> impl IntoElement {
        probe(SpotifyState::Connected(Playback {
            shuffle: true,
            smart_shuffle: true,
            repeat: crate::spotify::RepeatMode::Track,
            ..controllable()
        }))
    }

    #[component]
    fn ProbePausedNoDevice() -> impl IntoElement {
        probe(SpotifyState::Connected(Playback {
            device: None,
            title: Some("Paused Song".into()),
            artists: Some("Someone".into()),
            is_playing: false,
            can_control: true,
            duration_ms: 180_000,
            progress_ms: 30_000,
            ..Playback::default()
        }))
    }

    #[component]
    fn ProbeSeekable() -> impl IntoElement {
        probe(SpotifyState::Connected(controllable()))
    }

    fn many_playlists(n: usize) -> Vec<PlaylistSummary> {
        (0..n)
            .map(|i| PlaylistSummary {
                uri: if i == 0 { "spotify:user:me:collection".to_string() } else { format!("spotify:playlist:{i}") },
                name: if i == 0 { "Liked Songs".to_string() } else { format!("Playlist {i}") },
                image_url: None,
                can_add_tracks: true,
                song_count: Some(i as u32 * 3),
            })
            .collect()
    }

    fn playlist(uri: &str, name: &str, can_add: bool) -> PlaylistSummary {
        PlaylistSummary {
            uri: uri.to_string(),
            name: name.to_string(),
            image_url: None,
            can_add_tracks: can_add,
            song_count: None,
        }
    }

    /// Liked Songs, Spotify DJ (not addable), two editable playlists and one followed (read only).
    fn sheet_playlists() -> Vec<PlaylistSummary> {
        vec![
            playlist("spotify:user:me:collection", "Liked Songs", true),
            playlist("spotify:playlist:37i9dQZF1EYkqdzj48dyYq", "DJ", false),
            playlist("spotify:playlist:road", "Road Trip", true),
            playlist("spotify:playlist:focus", "Deep Focus", true),
            playlist("spotify:playlist:readonly", "Followed Mix", false),
        ]
    }

    fn with_track() -> Playback {
        Playback {
            track_uri: Some("spotify:track:abc123".into()),
            volume_percent: Some(40),
            can_set_volume: true,
            ..controllable()
        }
    }

    #[component]
    fn ProbeWithPlaylists() -> impl IntoElement {
        let s = signal(SpotifyState::Connected(controllable()));
        let list = signal(many_playlists(20));
        view().flex_1().p(px(24.0)).child(music_card(s, list, test_hooks(s)))
    }

    #[component]
    fn FullTimerOpen() -> impl IntoElement {
        let s = signal(SpotifyState::Connected(with_track()));
        dashboard_content(s, signal(many_playlists(12)), test_hooks(s), true)
    }

    #[component]
    fn FullTimerClosed() -> impl IntoElement {
        let s = signal(SpotifyState::Connected(with_track()));
        dashboard_content(s, signal(many_playlists(12)), test_hooks(s), false)
    }

    #[component]
    fn SheetDashboard() -> impl IntoElement {
        let s = signal(SpotifyState::Connected(with_track()));
        let hooks = test_hooks(s);
        // The test hands over a new Spotify state through `NEXT_STATE`; the app picks it up here.
        set_interval(Duration::from_millis(30), move || {
            if let Some(next) = NEXT_STATE.lock().unwrap().take() {
                s.set(next);
            }
        });
        dashboard_content(s, signal(sheet_playlists()), hooks, false)
    }

    #[component]
    fn SheetDashboardTimerOpen() -> impl IntoElement {
        let s = signal(SpotifyState::Connected(with_track()));
        dashboard_content(s, signal(sheet_playlists()), test_hooks(s), true)
    }

    #[component]
    fn ProbeNoVolume() -> impl IntoElement {
        probe(SpotifyState::Connected(Playback { track_uri: Some("spotify:track:abc123".into()), ..controllable() }))
    }

    #[component]
    fn ProbeVolume() -> impl IntoElement {
        probe(SpotifyState::Connected(with_track()))
    }

    #[component]
    fn ProbeEpisode() -> impl IntoElement {
        probe(SpotifyState::Connected(Playback { track_uri: Some("spotify:episode:xyz".into()), ..controllable() }))
    }

    #[component]
    fn ProbeNamedTiles() -> impl IntoElement {
        let s = signal(SpotifyState::Connected(controllable()));
        let list = signal(vec![
            playlist("spotify:user:me:collection", "Liked Songs", true),
            playlist("spotify:playlist:37i9dQZF1EYkqdzj48dyYq", "DJ", false),
            playlist("spotify:playlist:a", "Road Trip", true),
            playlist("spotify:playlist:b", "Chill Lofi Study Beats Forever", true),
        ]);
        view().flex_1().p(px(24.0)).child(music_card(s, list, test_hooks(s)))
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
        app.wait_for("id:settings_button").unwrap();
        let nodes = app.screen().layout_nodes();
        let find = |id: &str| {
            nodes
                .iter()
                .find(|n| n.test_id.as_deref() == Some(id))
                .unwrap_or_else(|| panic!("no node {id}"))
                .bounds
        };
        let music = find("music_status");
        let controls = find("settings_button");
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
        app.wait_for("Open in Safari").unwrap();
        app.press("id:spotify_open_pair").unwrap();
        assert!(crate::debug_log::text().contains("https://spotify.com/pair?code=ABCD-1234"));
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
        // Playing, so the middle button offers Pause; all of them are off without control.
        app.wait_for("label:Pause").unwrap();
        for id in CONTROL_IDS {
            assert_eq!(app.screen().enabled(id).unwrap(), false, "{id} should be disabled");
        }
        // Sign out lives in the settings menu now, not on the Music card.
        assert_eq!(app.screen().count("id:spotify_sign_out").unwrap(), 0);
        assert_eq!(app.screen().count("text:Sign out").unwrap(), 0);
    }

    const CONTROL_IDS: [&str; 5] = [
        "id:spotify_shuffle",
        "id:spotify_prev",
        "id:spotify_play_pause",
        "id:spotify_next",
        "id:spotify_repeat",
    ];

    #[test]
    fn music_connected_enables_controls_when_allowed() {
        let mut app = Headless::start(landscape_config(Root::of::<ProbeConnectedControllable>())).unwrap();
        app.wait_for("Test Song").unwrap();
        app.wait_for("label:Play").unwrap();
        for id in CONTROL_IDS {
            assert_eq!(app.screen().enabled(id).unwrap(), true, "{id} should be enabled");
        }
        // Icons only: no words on the transport buttons.
        for word in ["Previous", "Next", "Shuffle off", "Repeat off"] {
            assert_eq!(app.screen().count(&format!("text:{word}")).unwrap(), 0, "{word} shown as text");
        }
        app.wait_for("label:Shuffle off").unwrap();
        app.wait_for("label:Repeat off").unwrap();
        app.press("id:spotify_play_pause").unwrap();
        app.press("id:spotify_shuffle").unwrap();
        app.press("id:spotify_repeat").unwrap();
    }

    #[test]
    fn shuffle_and_repeat_show_every_state() {
        let mut app = Headless::start(landscape_config(Root::of::<ProbeShuffleOn>())).unwrap();
        app.wait_for("label:Shuffle on").unwrap();
        app.wait_for("label:Repeat all").unwrap();
        let mut app = Headless::start(landscape_config(Root::of::<ProbeSmartShuffle>())).unwrap();
        app.wait_for("label:Smart Shuffle on").unwrap();
        app.wait_for("label:Repeat one").unwrap();
    }

    #[test]
    fn a_paused_track_with_no_active_device_is_still_shown() {
        let mut app = Headless::start(landscape_config(Root::of::<ProbePausedNoDevice>())).unwrap();
        app.wait_for("id:spotify_track").unwrap();
        app.wait_for("Paused Song").unwrap();
        app.wait_for("Paused").unwrap();
        app.wait_for("label:Play").unwrap();
        assert_eq!(app.screen().count("id:spotify_idle").unwrap(), 0);
        assert_eq!(app.screen().count("text:Nothing playing").unwrap(), 0);
        assert!(app.screen().enabled("id:spotify_play_pause").unwrap());
    }

    #[test]
    fn dragging_the_time_bar_seeks_and_shows_the_target_time() {
        let mut app = Headless::start(landscape_config(Root::of::<ProbeSeekable>())).unwrap();
        app.wait_for("id:spotify_seek").unwrap();
        app.wait_for("id:spotify_elapsed").unwrap();
        assert!(app.screen().texts().contains(&"00:00"), "starts at 0:00: {:?}", app.screen().texts());
        let bar = app.screen().layout_nodes().into_iter().find(|n| n.test_id.as_deref() == Some("spotify_seek")).unwrap().bounds;
        eprintln!("seek bar: {bar:?}");
        // From the middle of the bar to 100 points further right: past halfway of a 200 s track.
        app.drag("id:spotify_seek", nsh_core::geometry::point(px(100.0), px(0.0))).unwrap();
        let texts = app.screen().texts().into_iter().map(str::to_owned).collect::<Vec<_>>();
        let elapsed = texts
            .iter()
            .find(|t| t.len() == 5 && t.as_bytes()[2] == b':' && t.as_str() != "03:20")
            .cloned()
            .unwrap_or_default();
        eprintln!("elapsed after the drag: {elapsed}");
        let secs: u32 = elapsed[..2].parse::<u32>().unwrap() * 60 + elapsed[3..].parse::<u32>().unwrap();
        assert!(secs > 100 && secs <= 200, "seek target {elapsed} is not past the middle");
        // A seek is refused (and the bar stays put) while controls are locked.
        let mut app = Headless::start(landscape_config(Root::of::<ProbeConnectedLocked>())).unwrap();
        app.wait_for("id:spotify_seek").unwrap();
        app.drag("id:spotify_seek", nsh_core::geometry::point(px(100.0), px(0.0))).unwrap();
        app.wait_for("01:01").unwrap();
    }

    #[test]
    fn playlists_show_as_a_row_of_covers_that_scrolls() {
        let mut app = Headless::start(landscape_config(Root::of::<ProbeWithPlaylists>())).unwrap();
        app.wait_for("id:spotify_playlists").unwrap();
        assert_eq!(app.screen().count("id:spotify_playlist").unwrap() >= 6, true);
        app.wait_for("label:Liked Songs").unwrap();
        let nodes = app.screen().layout_nodes();
        let row = nodes.iter().find(|n| n.test_id.as_deref() == Some("spotify_playlists")).unwrap();
        let music = nodes.iter().find(|n| n.test_id.as_deref() == Some("music_status")).unwrap();
        let scroll = row.scroll.as_ref().expect("the playlist row scrolls");
        eprintln!("playlist row {:?}, scroll {scroll:?}", row.bounds);
        assert!(row.bounds.origin.x.0 >= music.bounds.origin.x.0 - 0.5);
        assert!(row.bounds.origin.x.0 + row.bounds.size.width.0 <= music.bounds.origin.x.0 + music.bounds.size.width.0 + 0.5);
        // 20 covers do not fit: the row scrolls sideways to reach them.
        app.scroll("id:spotify_playlists", px(300.0)).unwrap();
        // Tapping a cover plays it.
        app.press("label:Liked Songs").unwrap_or(());
        app.press("id:spotify_playlist").unwrap_err();
    }

    #[test]
    fn playlists_are_hidden_when_there_are_none() {
        let mut app = Headless::start(landscape_config(Root::of::<ProbeConnectedControllable>())).unwrap();
        app.wait_for("Test Song").unwrap();
        assert_eq!(app.screen().count("id:spotify_playlists").unwrap(), 0);
    }

    // ----- Layout: nothing overlaps or leaves the screen, timer open or not -----

    type Rect = (f32, f32, f32, f32);

    fn rects(app: &Headless, ids: &[&str]) -> Vec<(String, Rect)> {
        let nodes = app.screen().layout_nodes();
        ids.iter()
            .map(|id| {
                let n = nodes
                    .iter()
                    .find(|n| n.test_id.as_deref() == Some(*id))
                    .unwrap_or_else(|| panic!("no node {id}"));
                let b = n.bounds;
                (id.to_string(), (b.origin.x.0, b.origin.y.0, b.size.width.0, b.size.height.0))
            })
            .collect()
    }

    fn inside(r: Rect, outer: Rect) -> bool {
        r.0 >= outer.0 - 0.5 && r.1 >= outer.1 - 0.5 && r.0 + r.2 <= outer.0 + outer.2 + 0.5 && r.1 + r.3 <= outer.1 + outer.3 + 0.5
    }

    fn overlaps(a: Rect, b: Rect) -> bool {
        a.0 < b.0 + b.2 - 0.5 && b.0 < a.0 + a.2 - 0.5 && a.1 < b.1 + b.3 - 0.5 && b.1 < a.1 + a.3 - 0.5
    }

    const MUSIC_CONTROLS: [&str; 7] = [
        "spotify_shuffle",
        "spotify_prev",
        "spotify_play_pause",
        "spotify_next",
        "spotify_repeat",
        "spotify_seek",
        "spotify_playlists",
    ];
    const TIMER_CONTROLS: [&str; 6] = [
        "timer_toggle",
        "timer_reset",
        "timer_preset_5",
        "timer_preset_45",
        "timer_editor",
        "timer_panel",
    ];

    fn check_no_overlap(app: &Headless, w: f32, h: f32, timer_open: bool) {
        let window = (0.0, 0.0, w, h);
        let mut ids: Vec<&str> = MUSIC_CONTROLS.to_vec();
        ids.extend(["music_status", "spotify_art", "spotify_art_placeholder", "spotify_title", "clock_time", "settings_button"]);
        if timer_open {
            ids.extend(TIMER_CONTROLS);
        }
        let ids: Vec<&str> = ids
            .into_iter()
            .filter(|id| app.screen().count(&format!("id:{id}")).unwrap_or(0) > 0)
            .collect();
        let found = rects(app, &ids);
        let get = |id: &str| found.iter().find(|(i, _)| i == id).map(|(_, r)| *r);
        for (id, r) in &found {
            eprintln!("  {id:>26}: x {:7.1} y {:7.1} w {:7.1} h {:7.1}", r.0, r.1, r.2, r.3);
        }
        // Everything is on the screen.
        for (id, r) in &found {
            assert!(inside(*r, window), "{id} {r:?} leaves the {w}x{h} window");
        }
        // The music controls stay inside the Music card.
        let card = get("music_status").unwrap();
        for id in MUSIC_CONTROLS {
            if let Some(r) = get(id) {
                assert!(inside(r, card), "{id} {r:?} leaves the music card {card:?}");
            }
        }
        // The artwork and the controls do not overlap each other.
        let art = get("spotify_art").or_else(|| get("spotify_art_placeholder"));
        if let Some(art) = art {
            for id in MUSIC_CONTROLS {
                if let Some(r) = get(id) {
                    assert!(!overlaps(art, r), "artwork {art:?} overlaps {id} {r:?}");
                }
            }
        }
        // The transport buttons and the seek bar do not overlap one another.
        for (i, a) in MUSIC_CONTROLS[..6].iter().enumerate() {
            for b in &MUSIC_CONTROLS[i + 1..6] {
                if let (Some(ra), Some(rb)) = (get(a), get(b)) {
                    assert!(!overlaps(ra, rb), "{a} {ra:?} overlaps {b} {rb:?}");
                }
            }
        }
        if timer_open {
            let panel = get("timer_panel").unwrap();
            assert!(!overlaps(card, panel), "music card {card:?} overlaps the timer panel {panel:?}");
            // (While the countdown runs the picker is replaced by the big time, so not every
            // timer control is always on screen.)
            for id in MUSIC_CONTROLS.iter().chain(["spotify_title"].iter()) {
                if let Some(m) = get(id) {
                    for t in TIMER_CONTROLS {
                        if let Some(tr) = get(t) {
                            assert!(!overlaps(m, tr), "{id} {m:?} overlaps {t} {tr:?}");
                        }
                    }
                }
            }
            for t in TIMER_CONTROLS {
                if let Some(tr) = get(t) {
                    assert!(inside(tr, panel), "{t} {tr:?} leaves the timer panel {panel:?}");
                }
            }
        }
    }

    #[test]
    fn landscape_with_the_timer_open_has_no_overlap() {
        let mut app = Headless::start(landscape_config(Root::of::<FullTimerOpen>())).unwrap();
        app.wait_for("id:timer_panel").unwrap();
        app.wait_for("id:spotify_play_pause").unwrap();
        eprintln!("1024x768, timer open, track playing, playlists shown:");
        check_no_overlap(&app, 1024.0, 768.0, true);
        // Running countdown keeps the same panel size and still does not touch the music card.
        app.press("id:timer_toggle").unwrap();
        app.wait_for("id:timer_remaining").unwrap();
        check_no_overlap(&app, 1024.0, 768.0, true);
    }

    #[test]
    fn landscape_with_the_timer_closed_has_no_overlap() {
        let mut app = Headless::start(landscape_config(Root::of::<FullTimerClosed>())).unwrap();
        app.wait_for("id:spotify_play_pause").unwrap();
        eprintln!("1024x768, timer closed:");
        check_no_overlap(&app, 1024.0, 768.0, false);
        // Opening the timer from the header gives the same safe layout.
        app.press("id:timer_open").unwrap();
        app.wait_for("id:timer_panel").unwrap();
        eprintln!("1024x768, timer just opened:");
        check_no_overlap(&app, 1024.0, 768.0, true);
        app.press("id:timer_open").unwrap();
        app.wait_for_gone("id:timer_panel", Duration::from_secs(5)).unwrap();
        check_no_overlap(&app, 1024.0, 768.0, false);
    }

    #[test]
    fn portrait_with_the_timer_open_has_no_overlap() {
        let mut config = EngineConfig::new(Root::of::<FullTimerOpen>());
        config.viewport = Size::new(Points(768.0), Points(1024.0));
        config.environment.window = config.viewport;
        let mut app = Headless::start(config).unwrap();
        app.wait_for("id:timer_panel").unwrap();
        app.wait_for("id:spotify_play_pause").unwrap();
        eprintln!("768x1024 portrait, timer open:");
        // Portrait scrolls, so only check that rows do not overlap and nothing is wider than the window.
        let found = rects(&app, &["spotify_play_pause", "spotify_seek", "timer_panel", "music_status", "timer_toggle", "spotify_shuffle", "spotify_repeat"]);
        for (id, r) in &found {
            eprintln!("  {id:>26}: x {:7.1} y {:7.1} w {:7.1} h {:7.1}", r.0, r.1, r.2, r.3);
            assert!(r.0 >= -0.5 && r.0 + r.2 <= 768.5, "{id} {r:?} is wider than the window");
        }
        let get = |id: &str| found.iter().find(|(i, _)| i == id).unwrap().1;
        assert!(!overlaps(get("music_status"), get("timer_panel")), "card and timer overlap in portrait");
        let (shuffle, repeat, play) = (get("spotify_shuffle"), get("spotify_repeat"), get("spotify_play_pause"));
        assert!(!overlaps(shuffle, play) && !overlaps(repeat, play));
    }

    // ----- Settings -----

    #[test]
    fn the_header_has_only_timer_and_settings() {
        let mut app = start_landscape();
        app.wait_for("id:settings_button").unwrap();
        app.wait_for("id:timer_open").unwrap();
        for gone in ["id:night_dim_toggle", "id:hour_cycle_toggle", "text:Dim", "text:24h", "text:12h"] {
            assert_eq!(app.screen().count(gone).unwrap(), 0, "{gone} is still in the header");
        }
        assert_eq!(app.screen().count("id:settings_menu").unwrap(), 0);
    }

    #[test]
    fn settings_menu_holds_dim_clock_debug_and_the_account() {
        let mut app = Headless::start(landscape_config(Root::of::<FullTimerClosed>())).unwrap();
        app.wait_for("id:settings_button").unwrap();
        app.press("id:settings_button").unwrap();
        app.wait_for("id:settings_menu").unwrap();
        for id in ["id:settings_night_dim", "id:settings_24h", "id:settings_debug", "id:settings_sign_out"] {
            app.wait_for(id).unwrap();
        }
        let rs = rects(&app, &["settings_panel", "settings_button"]);
        eprintln!("settings panel {:?}", rs[0].1);
        assert!(inside(rs[0].1, (0.0, 0.0, 1024.0, 768.0)));
        // Night dim switch.
        let checked = |app: &Headless, id: &str| app.screen().accessibility_state(id).unwrap().checked;
        assert_eq!(checked(&app, "id:settings_night_dim"), Some(Checked::False));
        app.press("id:settings_night_dim").unwrap();
        assert_eq!(checked(&app, "id:settings_night_dim"), Some(Checked::True));
        // 24-hour clock switch.
        let before = checked(&app, "id:settings_24h");
        app.press("id:settings_24h").unwrap();
        assert_ne!(checked(&app, "id:settings_24h"), before);
        // Tapping outside closes the menu.
        app.press("id:settings_backdrop").unwrap();
        app.wait_for_gone("id:settings_menu", Duration::from_secs(5)).unwrap();
        // The gear again; Debug log opens the existing overlay and closes the menu.
        app.press("id:settings_button").unwrap();
        app.press("id:settings_debug").unwrap();
        app.wait_for("id:debug_overlay").unwrap();
        assert_eq!(app.screen().count("id:settings_menu").unwrap(), 0);
        app.press("id:debug_close").unwrap();
        app.wait_for_gone("id:debug_overlay", Duration::from_secs(5)).unwrap();
        // Sign out from the menu returns to the signed-out card.
        app.press("id:settings_button").unwrap();
        app.press("id:settings_sign_out").unwrap();
        app.wait_for_gone("id:settings_menu", Duration::from_secs(5)).unwrap();
        app.wait_for("id:spotify_signed_out").unwrap();
        app.press("id:settings_button").unwrap();
        app.wait_for("id:settings_sign_in").unwrap();
        assert_eq!(app.screen().count("id:settings_sign_out").unwrap(), 0);
    }

    #[test]
    fn holding_the_gear_toggles_night_dim_and_a_tap_opens_the_menu() {
        let mut app = start_landscape();
        app.wait_for("id:settings_button").unwrap();
        app.long_press("id:settings_button").unwrap();
        // A long press is not a tap: the menu stays closed...
        assert_eq!(app.screen().count("id:settings_menu").unwrap(), 0);
        // ...and dim is now on.
        app.press("id:settings_button").unwrap();
        app.wait_for("id:settings_menu").unwrap();
        assert_eq!(
            app.screen().accessibility_state("id:settings_night_dim").unwrap().checked,
            Some(Checked::True)
        );
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

    // ----- Playlist tiles, volume, add to playlist -----

    #[test]
    fn short_names_stay_text_and_long_ones_become_initials() {
        assert_eq!(short_label("Road Trip"), ("Road Trip".to_string(), false));
        assert_eq!(short_label("Gym"), ("Gym".to_string(), false));
        assert_eq!(short_label("Chill Lofi Study Beats Forever"), ("CLS".to_string(), true));
        assert_eq!(short_label("Supercalifragilistic"), ("SUP".to_string(), true));
        assert_eq!(short_label("   "), ("?".to_string(), true));
        assert_eq!(initials("my daily mix 1"), "MDM");
    }

    #[test]
    fn the_sheet_lists_only_places_a_song_can_go() {
        let all = sheet_playlists();
        let names = |q: &str| add_targets(&all, q).into_iter().map(|p| p.name).collect::<Vec<_>>();
        assert_eq!(names(""), ["Liked Songs", "Road Trip", "Deep Focus"]);
        assert_eq!(names("  road "), ["Road Trip"]);
        assert!(names("dj").is_empty(), "DJ must never be a target");
        assert!(names("followed").is_empty(), "read-only playlists are not targets");
        // DJ is excluded even if a backend wrongly says it is addable.
        let mut dj = playlist("spotify:playlist:37i9dQZF1EYkqdzj48dyYq", "DJ", true);
        dj.can_add_tracks = true;
        assert!(add_targets(&[dj], "").is_empty());
        assert!(is_track_uri("spotify:track:abc") && !is_track_uri("spotify:episode:abc") && !is_track_uri("spotify:track:"));
        assert_eq!(volume_at(50.0, 200.0), 25);
        assert_eq!(volume_at(-5.0, 200.0), 0);
        assert_eq!(volume_at(999.0, 200.0), 100);
    }

    #[test]
    fn playlists_without_a_cover_show_a_name_liked_a_heart_and_dj_its_tile() {
        let mut app = Headless::start(landscape_config(Root::of::<ProbeNamedTiles>())).unwrap();
        app.wait_for("id:spotify_playlists").unwrap();
        assert_eq!(app.screen().count("id:playlist_tile_liked").unwrap(), 1);
        assert_eq!(app.screen().count("id:playlist_tile_dj").unwrap(), 1);
        assert_eq!(app.screen().count("id:playlist_tile_name").unwrap(), 2);
        // Short names are readable; the long one is shortened to initials.
        app.wait_for("text:Road Trip").unwrap();
        app.wait_for("text:CLS").unwrap();
        assert_eq!(app.screen().count("text:Chill Lofi Study Beats Forever").unwrap(), 0);
        // Everyone still has the full name for VoiceOver.
        app.wait_for("label:Chill Lofi Study Beats Forever").unwrap();
        app.wait_for("label:Liked Songs").unwrap();
        app.wait_for("label:DJ").unwrap();
    }

    #[test]
    fn the_volume_slider_sends_once_on_release_and_only_when_supported() {
        let _fake = fake_lock();
        let mut app = Headless::start(landscape_config(Root::of::<ProbeVolume>())).unwrap();
        app.wait_for("id:spotify_volume").unwrap();
        app.wait_for("label:Volume on Windows PC").unwrap();
        app.wait_for("text:40%").unwrap();
        // A drag of many moves sends one value, at the end.
        app.drag("id:spotify_volume", nsh_core::geometry::point(px(60.0), px(0.0))).unwrap();
        let sent = VOLUMES.lock().unwrap().clone();
        assert_eq!(sent.len(), 1, "one command per gesture, got {sent:?}");
        assert!(sent[0] > 50 && sent[0] <= 100, "dragged right from the middle: {sent:?}");
        // The slider shows what was asked for straight away.
        app.wait_for(&format!("text:{}%", sent[0])).unwrap();

        // No volume support: no slider at all.
        let app = Headless::start(landscape_config(Root::of::<ProbeNoVolume>())).unwrap();
        assert_eq!(app.screen().count("id:spotify_volume").unwrap(), 0);
        assert_eq!(app.screen().count("id:spotify_volume_row").unwrap(), 0);
    }

    #[test]
    fn the_plus_is_off_without_a_spotify_track() {
        let mut app = Headless::start(landscape_config(Root::of::<ProbeConnectedControllable>())).unwrap();
        app.wait_for("id:spotify_add").unwrap();
        assert!(!app.screen().enabled("id:spotify_add").unwrap(), "no track uri");
        let mut app = Headless::start(landscape_config(Root::of::<ProbeEpisode>())).unwrap();
        app.wait_for("id:spotify_add").unwrap();
        assert!(!app.screen().enabled("id:spotify_add").unwrap(), "podcast episodes cannot be saved here");
        let mut app = Headless::start(landscape_config(Root::of::<ProbeVolume>())).unwrap();
        app.wait_for("id:spotify_add").unwrap();
        assert!(app.screen().enabled("id:spotify_add").unwrap());
    }

    fn open_sheet(app: &mut Headless) {
        app.wait_for("id:spotify_add").unwrap();
        app.press("id:spotify_add").unwrap();
        app.wait_for("id:add_sheet_panel").unwrap();
        app.wait_for("id:add_sheet_row").unwrap();
    }

    #[test]
    fn the_add_sheet_hides_dj_marks_saved_and_saves_only_new_picks() {
        let _fake = fake_lock();
        ALREADY.lock().unwrap().push("spotify:user:me:collection".to_string());
        let mut app = Headless::start(landscape_config(Root::of::<SheetDashboard>())).unwrap();
        open_sheet(&mut app);
        // Opening asks the backend about this exact song.
        assert_eq!(INSPECTED.lock().unwrap().clone(), ["spotify:track:abc123"]);
        app.wait_for("text:Add to playlist").unwrap();
        // The song name is in the sheet; DJ and read-only playlists are not.
        assert_eq!(app.screen().count("id:add_sheet_row").unwrap(), 3);
        assert_eq!(app.screen().count("id:add_sheet_panel >> label:DJ").unwrap(), 0);
        assert_eq!(app.screen().count("id:add_sheet_panel >> label:Followed Mix").unwrap(), 0);
        // Liked Songs already has it: "Saved", cannot be picked again.
        app.wait_for("text:Saved").unwrap();
        let liked = app.screen().accessibility_state("id:add_sheet_panel >> label:Liked Songs").unwrap();
        assert_eq!(liked.checked, Some(Checked::True));
        assert!(!app.screen().enabled("id:add_sheet_panel >> label:Liked Songs").unwrap(), "saved row must not be pickable");
        assert!(app.screen().enabled("id:add_sheet_panel >> label:Road Trip").unwrap());

        // Search narrows the list.
        app.type_text("id:add_sheet_search", "road").unwrap();
        assert_eq!(app.screen().count("id:add_sheet_row").unwrap(), 1);
        app.wait_for("id:add_sheet_panel >> label:Road Trip").unwrap();
        app.type_text("id:add_sheet_search", "zzz").unwrap();
        // "roadzzz" matches nothing.
        app.wait_for("id:add_sheet_empty").unwrap();
        assert_eq!(app.screen().count("id:add_sheet_row").unwrap(), 0);
    }

    #[test]
    fn done_saves_to_the_picked_places_and_waits_for_the_backend_before_saying_so() {
        let _fake = fake_lock();
        let mut app = Headless::start(landscape_config(Root::of::<SheetDashboard>())).unwrap();
        open_sheet(&mut app);
        app.press("id:add_sheet_panel >> label:Road Trip").unwrap();
        app.press("id:add_sheet_panel >> label:Deep Focus").unwrap();
        app.press("id:add_sheet_panel >> label:Deep Focus").unwrap();
        assert_eq!(app.screen().accessibility_state("id:add_sheet_panel >> label:Road Trip").unwrap().checked, Some(Checked::True));
        assert_eq!(app.screen().accessibility_state("id:add_sheet_panel >> label:Deep Focus").unwrap().checked, Some(Checked::False));
        // Nothing is claimed before Done.
        assert_eq!(app.screen().count("text:Added to your playlists").unwrap(), 0);
        app.press("id:add_sheet_done").unwrap();
        // The backend got this exact song and only the picked place.
        assert_eq!(
            SAVES.lock().unwrap().clone(),
            [("spotify:track:abc123".to_string(), vec!["spotify:playlist:road".to_string()])]
        );
        // While it works: busy, Done off. Success text comes from the backend's notice.
        app.wait_for("text:Saving\u{2026}").unwrap();
        assert!(!app.screen().enabled("id:add_sheet_done").unwrap_or(true));
        app.wait_for("text:Added to your playlists").unwrap();
        // The sheet closes itself after a moment.
        app.wait_for_gone("id:add_sheet", Duration::from_secs(6)).unwrap();
    }

    #[test]
    fn a_failed_save_shows_the_error_and_keeps_the_sheet_open() {
        let _fake = fake_lock();
        *FAIL_WITH.lock().unwrap() = Some("Spotify said no.".to_string());
        let mut app = Headless::start(landscape_config(Root::of::<SheetDashboard>())).unwrap();
        open_sheet(&mut app);
        app.press("id:add_sheet_panel >> label:Road Trip").unwrap();
        app.press("id:add_sheet_done").unwrap();
        app.wait_for("text:Spotify said no.").unwrap();
        // Still open after the time a success would have closed it; no success text.
        let _ = app.wait_for_gone("id:never_there", Duration::from_millis(1800));
        assert_eq!(app.screen().count("id:add_sheet").unwrap(), 1);
        assert_eq!(app.screen().count("text:Added to your playlists").unwrap(), 0);
        // Done works again so the user can retry.
        assert!(app.screen().enabled("id:add_sheet_done").unwrap());
    }

    #[test]
    fn cancel_closes_without_saving_and_done_with_no_picks_just_closes() {
        let _fake = fake_lock();
        let mut app = Headless::start(landscape_config(Root::of::<SheetDashboard>())).unwrap();
        open_sheet(&mut app);
        app.press("id:add_sheet_panel >> label:Road Trip").unwrap();
        app.press("id:add_sheet_cancel").unwrap();
        app.wait_for_gone("id:add_sheet", Duration::from_secs(5)).unwrap();
        assert!(SAVES.lock().unwrap().is_empty(), "cancel must not save");
        open_sheet(&mut app);
        app.press("id:add_sheet_done").unwrap();
        app.wait_for_gone("id:add_sheet", Duration::from_secs(5)).unwrap();
        assert!(SAVES.lock().unwrap().is_empty(), "nothing picked, nothing saved");
        // The dim area around the sheet is a Cancel button too (a headless press goes to the
        // middle, which the sheet covers, so only its existence is checked).
        open_sheet(&mut app);
        app.wait_for("id:add_sheet_backdrop").unwrap();
        assert!(SAVES.lock().unwrap().is_empty());
    }

    #[test]
    fn the_sheet_keeps_the_song_it_was_opened_for_when_the_track_changes() {
        let _fake = fake_lock();
        let mut app = Headless::start(landscape_config(Root::of::<SheetDashboard>())).unwrap();
        open_sheet(&mut app);
        // The next song starts while the sheet is open.
        *NEXT_STATE.lock().unwrap() = Some(SpotifyState::Connected(Playback {
            title: Some("Another Song".into()),
            track_uri: Some("spotify:track:other999".into()),
            ..with_track()
        }));
        app.wait_for("text:Another Song").unwrap();
        app.wait_for("id:add_sheet").unwrap();
        app.press("id:add_sheet_panel >> label:Road Trip").unwrap();
        app.press("id:add_sheet_done").unwrap();
        let saved = SAVES.lock().unwrap().clone();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].0, "spotify:track:abc123", "must save the song the sheet opened for");
        // Its title in the sheet is the old one too.
        app.wait_for("id:add_sheet_track").unwrap();
        app.wait_for("text:Test Song").unwrap();
    }

    #[test]
    fn the_sheet_closes_if_spotify_goes_away() {
        let _fake = fake_lock();
        let mut app = Headless::start(landscape_config(Root::of::<SheetDashboard>())).unwrap();
        open_sheet(&mut app);
        *NEXT_STATE.lock().unwrap() = Some(SpotifyState::SignedOut);
        app.wait_for_gone("id:add_sheet", Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn the_sheet_and_the_new_controls_fit_with_the_timer_open() {
        let _fake = fake_lock();
        for (w, h) in [(1024.0_f32, 768.0_f32), (768.0, 1024.0)] {
            let mut config = EngineConfig::new(Root::of::<SheetDashboardTimerOpen>());
            config.viewport = Size::new(Points(w), Points(h));
            config.environment.window = config.viewport;
            let mut app = Headless::start(config).unwrap();
            app.wait_for("id:timer_panel").unwrap();
            app.wait_for("id:spotify_volume").unwrap();
            app.wait_for("id:spotify_add").unwrap();
            let landscape = w > h;
            let window = (0.0, 0.0, w, h);
            let ids = ["spotify_add", "spotify_volume", "spotify_volume_row", "spotify_title", "spotify_transport", "spotify_seek", "music_status", "timer_panel"];
            let found = rects(&app, &ids);
            let get = |id: &str| found.iter().find(|(i, _)| i == id).unwrap().1;
            eprintln!("{w}x{h} timer open:");
            for (id, r) in &found {
                eprintln!("  {id:>20}: x {:7.1} y {:7.1} w {:7.1} h {:7.1}", r.0, r.1, r.2, r.3);
            }
            if landscape {
                for (id, r) in &found {
                    assert!(inside(*r, window), "{id} {r:?} leaves the window");
                }
            }
            let card = get("music_status");
            for id in ["spotify_add", "spotify_volume", "spotify_volume_row", "spotify_transport", "spotify_seek"] {
                assert!(inside(get(id), card), "{id} {:?} leaves the music card {card:?}", get(id));
                assert!(!overlaps(get(id), get("timer_panel")), "{id} overlaps the timer panel");
            }
            // The plus sits beside the title, not over it, and the slider clears the buttons.
            assert!(!overlaps(get("spotify_add"), get("spotify_title")), "plus overlaps the title");
            assert!(!overlaps(get("spotify_volume_row"), get("spotify_transport")), "volume overlaps transport");
            assert!(!overlaps(get("spotify_volume_row"), get("spotify_seek")), "volume overlaps seek");
            // The plus is a comfortable touch target.
            assert!(get("spotify_add").2 >= 44.0 && get("spotify_add").3 >= 44.0);

            // The open sheet stays on the screen too (also with the timer panel behind it).
            app.press("id:spotify_add").unwrap();
            app.wait_for("id:add_sheet_panel").unwrap();
            let sheet = rects(&app, &["add_sheet_panel", "add_sheet_done", "add_sheet_cancel", "add_sheet_search"]);
            for (id, r) in &sheet {
                eprintln!("  {id:>20}: x {:7.1} y {:7.1} w {:7.1} h {:7.1}", r.0, r.1, r.2, r.3);
                assert!(inside(*r, window), "{id} {r:?} leaves the {w}x{h} window");
            }
            let panel = sheet[0].1;
            for (id, r) in &sheet[1..] {
                assert!(inside(*r, panel), "{id} {r:?} leaves the sheet {panel:?}");
            }
        }
    }
}
