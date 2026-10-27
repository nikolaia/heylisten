//! Keeps macOS from slowing heyListen down while it records or makes a note.
//!
//! heyListen runs in the background with no windows, which is exactly what App Nap throttles,
//! more so on battery. A user-started activity turns App Nap off for as long as it lasts, and
//! keeps the Mac from going to sleep in the middle of a meeting you're only listening to.

/// An activity macOS is told about; it ends when this is dropped.
pub struct Activity {
    #[cfg(target_os = "macos")]
    token: objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2::runtime::NSObjectProtocol>>,
}

impl Activity {
    pub fn begin(reason: &str) -> Activity {
        #[cfg(target_os = "macos")]
        {
            use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};
            let token = NSProcessInfo::processInfo().beginActivityWithOptions_reason(NSActivityOptions::UserInitiated, &NSString::from_str(reason));
            Activity { token }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = reason;
            Activity {}
        }
    }
}

impl Drop for Activity {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        unsafe {
            objc2_foundation::NSProcessInfo::processInfo().endActivity(&self.token);
        }
    }
}

/// Whether Low Power Mode is on: it slows transcription down a lot.
pub fn low_power_mode() -> bool {
    #[cfg(target_os = "macos")]
    {
        objc2_foundation::NSProcessInfo::processInfo().isLowPowerModeEnabled()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}
