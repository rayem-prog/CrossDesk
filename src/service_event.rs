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
}
