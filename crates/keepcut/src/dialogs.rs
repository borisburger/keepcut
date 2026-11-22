//! Notices when a native file dialog could not be shown.
//!
//! On Linux, rfd asks the desktop's file chooser portal (xdg-desktop-portal) for a dialog and
//! falls back to `zenity`. When neither is available (no portal backend or no session D-Bus,
//! e.g. in WSL or a minimal window manager; zenity isn't in the Flatpak), the dialog returns
//! "nothing chosen" and the button seems to do nothing. rfd logs the failure, so a tracing
//! layer watches for it and the app shows an explanation.

use std::sync::atomic::{AtomicBool, Ordering};

use tracing::{Event, Level, Subscriber};
use tracing_log::NormalizeEvent as _;
use tracing_subscriber::layer::{Context, Layer};

static PORTAL_FAILED: AtomicBool = AtomicBool::new(false);

/// Tracing layer that records rfd's portal warnings and errors.
pub struct PortalWatch;

impl<S: Subscriber> Layer<S> for PortalWatch {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        // rfd logs through the `log` crate; its records arrive with the target "log",
        // and the original target only in the normalized metadata.
        let normalized = event.normalized_metadata();
        let m = normalized.as_ref().unwrap_or_else(|| event.metadata());
        // tracing orders levels by verbosity: ERROR < WARN < INFO
        if *m.level() <= Level::WARN && m.target().starts_with("rfd::backend::xdg_desktop_portal") {
            PORTAL_FAILED.store(true, Ordering::Relaxed);
        }
    }
}

/// True once after a file dialog failed for lack of a portal.
pub fn take_portal_failure() -> bool {
    PORTAL_FAILED.swap(false, Ordering::Relaxed)
}

/// Explanation shown in the app.
pub const PORTAL_HELP: &str = "Keepcut couldn't open a file dialog. On Linux it asks the desktop's \
file chooser service (xdg-desktop-portal) for one, and none is available.\n\n\
Install a portal backend for your desktop (for example xdg-desktop-portal-gtk, -gnome or -kde) \
and make sure a session D-Bus is running, then try again. Meanwhile you can drop files onto the \
window, or open them from the command line:\n\n    flatpak run io.github.borisburger.Keepcut FILE…";

/// Video file extensions offered by the file dialogs.
pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mkv", "mov", "m4v", "ts", "avi", "webm"];

/// Extensions for a file dialog filter in lower and upper case: on Linux the portal/GTK filter
/// matches `*.mov` case-sensitively, so camera files named `.MOV`/`.MP4` would be hidden.
pub fn filter_extensions(exts: &[&str]) -> Vec<String> {
    exts.iter()
        .flat_map(|e| [e.to_ascii_lowercase(), e.to_ascii_uppercase()])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_extensions_has_both_cases() {
        assert_eq!(
            filter_extensions(&["mov", "ts"]),
            ["mov", "MOV", "ts", "TS"]
        );
    }
}
