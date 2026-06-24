//! A thin, fail-soft wrapper around the system clipboard.
//!
//! Clipboard access can be unavailable (e.g. a headless or WSL session without
//! WSLg). Rather than make yanking fallible everywhere, the sink degrades to a
//! no-op and the caller keeps the value in its in-app register.

/// Owns the platform clipboard handle for the lifetime of the app. On X11 the
/// handle must stay alive for a pasted value to remain available, so this is
/// constructed once and held in [`crate::app::state::App`].
pub struct ClipboardSink {
    inner: Option<arboard::Clipboard>,
}

impl ClipboardSink {
    /// Try to acquire the system clipboard; falls back to a disabled sink if
    /// none is available.
    pub fn new() -> ClipboardSink {
        ClipboardSink {
            inner: arboard::Clipboard::new().ok(),
        }
    }

    /// A sink that never touches a real clipboard (used in tests).
    #[cfg(test)]
    pub fn disabled() -> ClipboardSink {
        ClipboardSink { inner: None }
    }

    /// Copy `text` to the system clipboard. Returns `true` if it landed there;
    /// `false` if no clipboard is available or the write failed.
    pub fn copy(&mut self, text: &str) -> bool {
        match &mut self.inner {
            Some(clipboard) => clipboard.set_text(text.to_owned()).is_ok(),
            None => false,
        }
    }
}
