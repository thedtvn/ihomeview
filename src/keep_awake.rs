//! Keeps the iPad screen on while the dashboard is open (`UIApplication.idleTimerDisabled`).
//!
//! iOS turns the idle timer back on by itself when the app leaves the foreground, so setting it
//! at start and re-asserting it now and then is enough; nothing has to undo it.

/// Disables the idle timer (iOS only; a no-op elsewhere). Safe to call repeatedly.
pub fn keep_screen_on() {
    #[cfg(target_os = "ios")]
    {
        let _ = nsh_core::prelude::run_on_main_thread(|| {
            use objc2::runtime::{AnyClass, AnyObject, Bool};
            use objc2::msg_send;
            let Some(class) = AnyClass::get(c"UIApplication") else { return };
            // SAFETY: UIKit calls on the main thread; `sharedApplication` returns the app object.
            unsafe {
                let app: *mut AnyObject = msg_send![class, sharedApplication];
                if !app.is_null() {
                    let _: () = msg_send![app, setIdleTimerDisabled: Bool::YES];
                }
            }
        });
    }
}
