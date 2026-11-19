//! Keep-screen-on policy (`UIApplication.idleTimerDisabled`).
//!
//! Keeping the screen on drains the battery, so by default it only applies while the iPad is
//! charging (or full on the charger). The mode is a setting: while charging / always / off.
//! iOS re-enables the idle timer when the app leaves the foreground.
use std::cell::RefCell;

use nsh_core::{prelude::*, storage::Storage};

const KEY_MODE: &str = "keep_awake_mode";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeepAwake {
    /// Default: screen stays on only while plugged in.
    WhileCharging,
    Always,
    Off,
}

impl KeepAwake {
    pub fn label(self) -> &'static str {
        match self {
            KeepAwake::WhileCharging => "While charging",
            KeepAwake::Always => "Always",
            KeepAwake::Off => "Off",
        }
    }

    pub fn next(self) -> Self {
        match self {
            KeepAwake::WhileCharging => KeepAwake::Always,
            KeepAwake::Always => KeepAwake::Off,
            KeepAwake::Off => KeepAwake::WhileCharging,
        }
    }

    fn key(self) -> &'static str {
        match self {
            KeepAwake::WhileCharging => "charging",
            KeepAwake::Always => "always",
            KeepAwake::Off => "off",
        }
    }

    fn from_key(key: &str) -> Self {
        match key {
            "always" => KeepAwake::Always,
            "off" => KeepAwake::Off,
            _ => KeepAwake::WhileCharging,
        }
    }
}

thread_local! {
    static MODE: RefCell<Option<Signal<KeepAwake>>> = const { RefCell::new(None) };
}

/// The current mode (persisted). Same signal every call.
pub fn mode() -> Signal<KeepAwake> {
    MODE.with(|m| {
        *m.borrow_mut().get_or_insert_with(|| {
            let saved = Storage::get_item(KEY_MODE).map(|k| KeepAwake::from_key(&k)).unwrap_or(KeepAwake::WhileCharging);
            Owner::detached().run(|| signal(saved))
        })
    })
}

/// Switches to the next mode, saves it and applies it at once.
pub fn cycle_mode() {
    let next = mode().get_untracked().next();
    let _ = Storage::set_item(KEY_MODE, next.key());
    mode().set(next);
    apply();
}

/// Applies the mode now: decides on the main thread (battery state is a UIKit read).
pub fn apply() {
    let mode = mode().get_untracked();
    #[cfg(target_os = "ios")]
    {
        let _ = run_on_main_thread(move || {
            use objc2::msg_send;
            use objc2::runtime::{AnyClass, AnyObject, Bool};
            // SAFETY: UIKit calls on the main thread with valid class/instance pointers.
            unsafe {
                let charging = match AnyClass::get(c"UIDevice") {
                    Some(class) => {
                        let device: *mut AnyObject = msg_send![class, currentDevice];
                        if device.is_null() {
                            false
                        } else {
                            let _: () = msg_send![device, setBatteryMonitoringEnabled: Bool::YES];
                            // UIDeviceBatteryState: 0 unknown, 1 unplugged, 2 charging, 3 full.
                            let state: isize = msg_send![device, batteryState];
                            state == 2 || state == 3
                        }
                    }
                    None => false,
                };
                let keep_on = match mode {
                    KeepAwake::Always => true,
                    KeepAwake::WhileCharging => charging,
                    KeepAwake::Off => false,
                };
                if let Some(class) = AnyClass::get(c"UIApplication") {
                    let app: *mut AnyObject = msg_send![class, sharedApplication];
                    if !app.is_null() {
                        let _: () = msg_send![app, setIdleTimerDisabled: Bool::new(keep_on)];
                    }
                }
            }
        });
    }
    #[cfg(not(target_os = "ios"))]
    let _ = mode;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_cycle_and_round_trip() {
        assert_eq!(KeepAwake::WhileCharging.next(), KeepAwake::Always);
        assert_eq!(KeepAwake::Always.next(), KeepAwake::Off);
        assert_eq!(KeepAwake::Off.next(), KeepAwake::WhileCharging);
        for m in [KeepAwake::WhileCharging, KeepAwake::Always, KeepAwake::Off] {
            assert_eq!(KeepAwake::from_key(m.key()), m);
        }
        assert_eq!(KeepAwake::from_key("garbage"), KeepAwake::WhileCharging);
    }
}
