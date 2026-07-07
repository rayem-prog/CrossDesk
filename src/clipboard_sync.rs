use std::time::{Duration, Instant};

use crate::protocol::{self, ClipboardPayload};

const REMOTE_CLIPBOARD_ECHO_WINDOW: Duration = Duration::from_millis(1200);

/// Tracks clipboard payloads seen on this side of the connection.
///
/// When a remote clipboard payload is written locally, the OS clipboard watcher
/// often reports it back as a fresh local change. This state prevents that echo
/// from being sent straight back to the peer while still allowing genuine local
/// edits to pass through.
#[derive(Default)]
pub struct ClipboardSyncState {
    last_observed_payload: Option<Vec<u8>>,
    pending_echo_kind: Option<ClipboardKind>,
    echo_suppression_deadline: Option<Instant>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ClipboardKind {
    Text,
    Bitmap,
    Files,
}

impl ClipboardSyncState {
    pub fn accept_local_change(&mut self, payload: &ClipboardPayload, encoded: Vec<u8>) -> bool {
        if self.last_observed_payload.as_ref() == Some(&encoded) {
            return false;
        }

        let payload_kind = ClipboardKind::from_payload(payload);
        if self.should_suppress_echo(payload_kind) {
            eprintln!("suppressed remote clipboard echo");
            self.last_observed_payload = Some(encoded);
            return false;
        }

        self.clear_echo_suppression();
        self.last_observed_payload = Some(encoded);
        true
    }

    pub fn note_remote_write(&mut self, payload: &ClipboardPayload) {
        self.last_observed_payload = Some(protocol::encode_clipboard(payload));
        self.pending_echo_kind = Some(ClipboardKind::from_payload(payload));
        self.echo_suppression_deadline = Some(Instant::now() + REMOTE_CLIPBOARD_ECHO_WINDOW);
    }

    fn should_suppress_echo(&mut self, payload_kind: ClipboardKind) -> bool {
        if self.pending_echo_kind != Some(payload_kind) {
            return false;
        }

        let echo_window_is_open = self
            .echo_suppression_deadline
            .is_some_and(|deadline| Instant::now() <= deadline);
        self.clear_echo_suppression();
        echo_window_is_open
    }

    fn clear_echo_suppression(&mut self) {
        self.pending_echo_kind = None;
        self.echo_suppression_deadline = None;
    }
}

impl ClipboardKind {
    fn from_payload(payload: &ClipboardPayload) -> Self {
        match payload {
            ClipboardPayload::Text(_) => Self::Text,
            ClipboardPayload::Bitmap(_) => Self::Bitmap,
            ClipboardPayload::Files(_) => Self::Files,
        }
    }
}

pub fn clipboard_summary(payload: &ClipboardPayload) -> String {
    match payload {
        ClipboardPayload::Text(text) => format!("text ({} chars)", text.chars().count()),
        ClipboardPayload::Bitmap(bitmap) => format!("image ({} bytes)", bitmap.len()),
        ClipboardPayload::Files(files) => format!("files ({} item(s))", files.len()),
    }
}
