// Structured local IPC; human logs never establish a connection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "gui", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "gui", serde(rename_all = "snake_case"))]
pub enum ServiceState {
    #[default]
    Stopped,
    Starting,
    Listening,
    Connecting,
    Connected,
    Retrying,
    Stopping,
    Error,
}
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "gui", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "gui", serde(deny_unknown_fields))]
pub struct ServiceEvent {
    pub state: ServiceState,
    pub message: Option<String>,
}
#[cfg(feature = "gui")]
pub fn encode(event: &ServiceEvent) -> String {
    format!(
        "crossdesk-event\t{}",
        serde_json::to_string(event).expect("serializable status")
    )
}
pub fn parse(line: &str) -> Option<ServiceEvent> {
    #[cfg(feature = "gui")]
    {
        if line.len() > 4096 {
            return None;
        }
        serde_json::from_str(line.strip_prefix("crossdesk-event\t")?).ok()
    }
    #[cfg(not(feature = "gui"))]
    {
        let _ = line;
        None
    }
}
pub fn emit(state: ServiceState, message: Option<String>) {
    #[cfg(feature = "gui")]
    {
        use std::io::Write;
        if std::env::var_os("DESKBRIDGE_GUI_CHILD").is_some() {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{}", encode(&ServiceEvent { state, message }));
            let _ = out.flush();
        }
    }
    #[cfg(not(feature = "gui"))]
    {
        let _ = (state, message);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "gui", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "gui", serde(rename_all = "snake_case"))]
pub enum TransferState {
    Preparing,
    Transferring,
    Waiting,
    Ready,
    Skipped,
    Failed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "gui", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "gui", serde(rename_all = "snake_case"))]
pub enum TransferDirection {
    Send,
    Receive,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "gui", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "gui", serde(rename_all = "snake_case"))]
pub enum TransferKind {
    #[default]
    Files,
    Text,
}
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "gui", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "gui", serde(deny_unknown_fields))]
pub struct TransferEvent {
    #[cfg_attr(feature = "gui", serde(default))]
    pub kind: TransferKind,
    pub id: String,
    pub direction: TransferDirection,
    pub state: TransferState,
    pub message: Option<String>,
}
#[cfg(feature = "gui")]
pub fn encode_transfer(event: &TransferEvent) -> String {
    format!(
        "crossdesk-transfer\t{}",
        serde_json::to_string(event).unwrap()
    )
}
pub fn parse_transfer(line: &str) -> Option<TransferEvent> {
    #[cfg(feature = "gui")]
    {
        if line.len() > 4096 {
            return None;
        }
        let event: TransferEvent =
            serde_json::from_str(line.strip_prefix("crossdesk-transfer\t")?).ok()?;
        if event.id.len() != 32
            || !event.id.bytes().all(|c| c.is_ascii_hexdigit())
            || event
                .message
                .as_ref()
                .is_some_and(|s| s.len() > 2048 || s.chars().any(char::is_control))
        {
            return None;
        }
        Some(event)
    }
    #[cfg(not(feature = "gui"))]
    {
        let _ = line;
        None
    }
}
pub fn transfer(
    id: [u8; 16],
    direction: TransferDirection,
    state: TransferState,
    message: Option<String>,
) {
    emit_transfer(TransferKind::Files, id, direction, state, message);
}
pub fn text_transfer(
    id: [u8; 16],
    direction: TransferDirection,
    state: TransferState,
    message: Option<String>,
) {
    emit_transfer(TransferKind::Text, id, direction, state, message);
}
fn emit_transfer(
    kind: TransferKind,
    id: [u8; 16],
    direction: TransferDirection,
    state: TransferState,
    message: Option<String>,
) {
    #[cfg(feature = "gui")]
    {
        use std::io::Write;
        if std::env::var_os("DESKBRIDGE_GUI_CHILD").is_some() {
            let event = TransferEvent {
                kind,
                id: id.iter().map(|v| format!("{v:02x}")).collect(),
                direction,
                state,
                message: message.map(|s| s.chars().filter(|c| !c.is_control()).take(500).collect()),
            };
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{}", encode_transfer(&event));
            let _ = out.flush();
        }
    }
    #[cfg(not(feature = "gui"))]
    {
        let _ = (kind, id, direction, state, message);
    }
}
#[derive(Default)]
pub struct TransferTracker {
    pub latest: Option<TransferEvent>,
    active: [Option<String>; 2],
    terminal: [bool; 2],
}
impl TransferTracker {
    pub fn apply(&mut self, event: TransferEvent) {
        let index = if event.direction == TransferDirection::Send {
            0
        } else {
            1
        };
        if self.active[index].as_ref() == Some(&event.id) && self.terminal[index] {
            return;
        }
        if matches!(
            event.state,
            TransferState::Preparing | TransferState::Transferring
        ) {
            self.active[index] = Some(event.id.clone());
            self.terminal[index] = false;
        }
        if self.active[index].as_ref() == Some(&event.id) {
            self.terminal[index] = matches!(
                event.state,
                TransferState::Ready | TransferState::Skipped | TransferState::Failed
            );
            self.latest = Some(event);
        }
    }
}
#[cfg(all(test, feature = "gui"))]
mod tests {
    use super::*;
    #[test]
    fn human_logs_and_bad_events_do_not_mean_connected() {
        assert!(parse("client ready (protocol v6)").is_none());
        assert!(parse("crossdesk-event\t{bad}").is_none());
        let event = ServiceEvent {
            state: ServiceState::Connected,
            message: None,
        };
        assert_eq!(parse(&encode(&event)), Some(event));
        assert!(parse("crossdesk-event\t{\"state\":\"unknown\"}").is_none());
    }
    #[test]
    fn transfer_failures_do_not_parse_as_connection_events_or_replace_a_new_copy() {
        let event = TransferEvent {
            kind: TransferKind::Files,
            id: "11".repeat(16),
            direction: TransferDirection::Send,
            state: TransferState::Preparing,
            message: None,
        };
        assert!(parse(&encode_transfer(&event)).is_none());
        assert_eq!(
            parse_transfer(&encode_transfer(&event)),
            Some(event.clone())
        );
        let mut tracker = TransferTracker::default();
        tracker.apply(event.clone());
        let next = TransferEvent {
            id: "22".repeat(16),
            ..event.clone()
        };
        tracker.apply(next.clone());
        tracker.apply(TransferEvent {
            state: TransferState::Failed,
            ..event
        });
        assert_eq!(tracker.latest, Some(next));
        assert!(parse_transfer("crossdesk-transfer\t{\"id\":\"bad\",\"direction\":\"send\",\"state\":\"ready\",\"message\":null}").is_none());
    }
    #[test]
    fn a_fast_publication_result_cannot_regress_to_waiting() {
        let event = TransferEvent {
            kind: TransferKind::Files,
            id: "33".repeat(16),
            direction: TransferDirection::Send,
            state: TransferState::Preparing,
            message: None,
        };
        let mut tracker = TransferTracker::default();
        tracker.apply(event.clone());
        tracker.apply(TransferEvent {
            state: TransferState::Ready,
            ..event.clone()
        });
        tracker.apply(TransferEvent {
            state: TransferState::Waiting,
            ..event
        });
        assert_eq!(tracker.latest.unwrap().state, TransferState::Ready);
    }
}

#[cfg(all(test, feature = "gui"))]
mod clipboard_capability_tests {
    use super::*;
    #[test]
    fn capability_ipc_is_strict_bounded_and_not_a_human_log() {
        assert_eq!(
            parse_capabilities("crossdesk-capabilities\t{\"text\":true}"),
            Some(ClipboardCapabilities { text: true })
        );
        assert_eq!(
            parse_capabilities("crossdesk-capabilities\t{\"text\":false}"),
            Some(ClipboardCapabilities { text: false })
        );
        for bad in [
            "text enabled",
            "crossdesk-capabilities\t{}",
            "crossdesk-capabilities\t{\"text\":1}",
            "crossdesk-capabilities\t{\"text\":true,\"other\":false}",
        ] {
            assert!(parse_capabilities(bad).is_none());
        }
        assert!(
            parse_capabilities(&format!(
                "crossdesk-capabilities\t{}{{\"text\":true}}",
                " ".repeat(4096)
            ))
            .is_none()
        );
    }
    #[test]
    fn old_file_events_default_to_files_and_text_events_keep_kind() {
        let old = "crossdesk-transfer\t{\"id\":\"11111111111111111111111111111111\",\"direction\":\"send\",\"state\":\"preparing\",\"message\":null}";
        let mut event = parse_transfer(old).unwrap();
        assert_eq!(event.kind, TransferKind::Files);
        event.kind = TransferKind::Text;
        assert_eq!(
            parse_transfer(&encode_transfer(&event)).unwrap().kind,
            TransferKind::Text
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "gui", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "gui", serde(deny_unknown_fields))]
pub struct ClipboardCapabilities {
    pub text: bool,
}
pub fn parse_capabilities(line: &str) -> Option<ClipboardCapabilities> {
    #[cfg(feature = "gui")]
    {
        if line.len() > 4096 {
            return None;
        }
        serde_json::from_str(line.strip_prefix("crossdesk-capabilities\t")?).ok()
    }
    #[cfg(not(feature = "gui"))]
    {
        let _ = line;
        None
    }
}
pub fn capabilities(text: bool) {
    #[cfg(feature = "gui")]
    {
        use std::io::Write;
        if std::env::var_os("DESKBRIDGE_GUI_CHILD").is_some() {
            let event = ClipboardCapabilities { text };
            let mut out = std::io::stdout().lock();
            let _ = writeln!(
                out,
                "crossdesk-capabilities\t{}",
                serde_json::to_string(&event).unwrap()
            );
            let _ = out.flush();
        }
    }
    #[cfg(not(feature = "gui"))]
    {
        let _ = text;
    }
}
