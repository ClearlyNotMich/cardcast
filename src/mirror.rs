//! mirror module. Windows calls live here.

// unsafe is opt-in per module; see Cargo.toml [lints].
#![allow(
    unsafe_code,
    reason = "SPEC allows unsafe only in mirror, guard and audio, for Windows API calls"
)]

use crate::guard::Monitor;

/// What the settings window needs from the mirror.
pub trait Mirror {
    /// Errors are plain words for the status line.
    fn start(&mut self, source_monitor: &Monitor, card_display: &Monitor) -> Result<(), String>;
    fn stop(&mut self);
}

/// Stand-in until the real mirror lands; it only remembers whether it was started.
#[derive(Debug, Default)]
pub struct StandInMirror {
    pub running: bool,
}

impl Mirror for StandInMirror {
    fn start(&mut self, _source_monitor: &Monitor, _card_display: &Monitor) -> Result<(), String> {
        self.running = true;
        Ok(())
    }

    fn stop(&mut self) {
        self.running = false;
    }
}
