//! Bounded local commands and snapshots. These messages never enter human logs.
use crate::edge_drop::{DropCommand, DropTransfer, DropView};
use std::io;

pub const COMMAND_PREFIX: &str = "crossdesk-drop-command\t";
pub const EVENT_PREFIX: &str = "crossdesk-drop\t";
pub const MAX_COMMAND_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_EVENT_BYTES: usize = 16 * 1024;

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "投送控制消息无效")
}
fn id_valid(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}
fn validate_command(command: &DropCommand) -> io::Result<()> {
    let valid = match command {
        DropCommand::Send { paths } => {
            !paths.is_empty() && paths.len() <= 64 && paths.iter().all(|p| p.is_absolute())
        }
        DropCommand::Accept { id, directory } => id_valid(id) && directory.is_absolute(),
        DropCommand::Reject { id } | DropCommand::Cancel { id } => id_valid(id),
        DropCommand::SetEnabled { .. } => true,
    };
    if valid { Ok(()) } else { Err(invalid()) }
}
pub fn encode_command(command: &DropCommand) -> io::Result<Vec<u8>> {
    validate_command(command)?;
    let json = serde_json::to_vec(command).map_err(|_| invalid())?;
    if json.len() + COMMAND_PREFIX.len() > MAX_COMMAND_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "投送路径清单过大",
        ));
    }
    let mut out = COMMAND_PREFIX.as_bytes().to_vec();
    out.extend(json);
    out.push(b'\n');
    Ok(out)
}
pub fn parse_command(line: &str) -> io::Result<Option<DropCommand>> {
    let Some(json) = line.strip_prefix(COMMAND_PREFIX) else {
        return Ok(None);
    };
    if line.len() > MAX_COMMAND_BYTES {
        return Err(invalid());
    }
    let command = serde_json::from_str(json).map_err(|_| invalid())?;
    validate_command(&command)?;
    Ok(Some(command))
}
fn transfer_valid(transfer: &DropTransfer, incoming: bool) -> bool {
    id_valid(&transfer.id)
        && matches!(
            transfer.state.as_str(),
            "preparing"
                | "waiting"
                | "sending"
                | "receiving"
                | "saved"
                | "rejected"
                | "cancelled"
                | "failed"
        )
        && transfer.total_bytes <= crate::file_transfer::MAX_BATCH_BYTES
        && transfer.transferred_bytes <= transfer.total_bytes
        && transfer.names.len() <= 64
        && transfer
            .names
            .iter()
            .all(|s| s.len() <= 1024 && !s.chars().any(char::is_control))
        && transfer
            .message
            .as_ref()
            .is_none_or(|s| s.len() <= 2048 && !s.chars().any(char::is_control))
        && transfer
            .saved_path
            .as_ref()
            .is_none_or(|p| incoming && transfer.state == "saved" && p.is_absolute())
}
pub fn parse_event(line: &str) -> Option<DropView> {
    if line.len() > MAX_EVENT_BYTES {
        return None;
    }
    let view: DropView = serde_json::from_str(line.strip_prefix(EVENT_PREFIX)?).ok()?;
    if !matches!(
        view.channel.as_str(),
        "disconnected" | "connecting" | "ready" | "unavailable"
    ) || view
        .outgoing
        .as_ref()
        .is_some_and(|t| !transfer_valid(t, false))
        || view
            .incoming
            .as_ref()
            .is_some_and(|t| !transfer_valid(t, true))
    {
        return None;
    }
    Some(view)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_paths_survive_quotes_newlines_and_chinese_without_injecting_commands() {
        let path = std::env::temp_dir().join("中文\"\nstop\n.pdf");
        let encoded = encode_command(&DropCommand::Send {
            paths: vec![path.clone()],
        })
        .unwrap();
        assert_eq!(encoded.iter().filter(|&&b| b == b'\n').count(), 1);
        let line = std::str::from_utf8(&encoded[..encoded.len() - 1]).unwrap();
        let Some(DropCommand::Send { paths }) = parse_command(line).unwrap() else {
            panic!("wrong command")
        };
        assert_eq!(paths, vec![path]);
    }
    #[test]
    fn commands_reject_relative_paths_unknown_variants_and_oversize_messages() {
        assert!(
            encode_command(&DropCommand::Send {
                paths: vec!["relative.pdf".into()]
            })
            .is_err()
        );
        assert!(parse_command("crossdesk-drop-command\t{\"type\":\"execute\"}").is_err());
        assert!(
            parse_command(&format!(
                "{COMMAND_PREFIX}{}",
                " ".repeat(MAX_COMMAND_BYTES)
            ))
            .is_err()
        );
        assert!(parse_command("stop").unwrap().is_none());
    }
    #[test]
    fn snapshots_reject_peer_paths_and_impossible_progress() {
        let prefix = "crossdesk-drop\t";
        let transfer = format!(
            r#"{{"id":"{}","state":"sending","names":["a.pdf"],"totalBytes":4,"transferredBytes":5,"message":null,"savedPath":null}}"#,
            "12".repeat(16)
        );
        let line = format!(
            r#"{prefix}{{"enabled":true,"peerSupported":true,"channel":"ready","outgoing":{transfer},"incoming":null}}"#
        );
        assert!(parse_event(&line).is_none());
        let line = line.replace("\"transferredBytes\":5", "\"transferredBytes\":4");
        assert!(parse_event(&line).is_some());
        let path = serde_json::to_string(&std::env::temp_dir()).unwrap();
        assert!(
            parse_event(&line.replace("\"savedPath\":null", &format!("\"savedPath\":{path}")))
                .is_none()
        );
    }
}
