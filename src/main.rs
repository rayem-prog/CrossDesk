#![allow(dead_code)]
mod client;
mod clipboard;
mod config;
mod control;
mod desktop;
mod file_transfer;
#[cfg(feature = "gui")]
mod gui;
mod image_transfer;
mod input;
#[cfg(target_os = "linux")]
mod linux;
mod platform;
mod pointer;
mod protocol;
mod secure;
mod server;
mod shutdown;
mod transport;

use std::{env, path::PathBuf, process::ExitCode};

#[derive(Debug)]
enum Command {
    Gui,
    Keygen { output: PathBuf },
    Server { bind: String, edge: server::Edge },
    Client { server: String },
}

fn main() -> ExitCode {
    let args: Vec<_> = env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage();
        return ExitCode::SUCCESS;
    }
    if args == ["--version"] {
        println!("CrossDesk {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let command = match parse_args(&args) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("{error}");
            print_usage();
            return ExitCode::FAILURE;
        }
    };
    if let Some(path) = value_after(&args, "--key-file") {
        // All environment setup precedes creation of any worker thread.
        unsafe {
            env::set_var("CROSSDESK_KEY_FILE", path);
        }
    }
    if let Some(value) = value_after(&args, "--image-clipboard") {
        unsafe {
            env::set_var("CROSSDESK_IMAGE_CLIPBOARD", value);
        }
    }
    if let Err(error) = shutdown::install_signal_handler() {
        eprintln!("Cannot install shutdown handler: {error}");
        return ExitCode::FAILURE;
    }
    let result = match command {
        Command::Keygen { output } => secure::generate_key(&output).map(|()| {
            println!("Created pairing key: {}\nCopy this same file to the Mac, beside its CrossDesk executable.", output.display());
        }),
        Command::Server { bind, edge } => server::run(server::ServerConfig { bind, edge }),
        Command::Client { server } => client::run(&server),
        Command::Gui => {
            #[cfg(feature = "gui")]
            { gui::run() }
            #[cfg(not(feature = "gui"))]
            { Err(std::io::Error::other("Use the supplied start scripts for this input prototype.")) }
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn parse_args(args: &[String]) -> Result<Command, String> {
    let verb = args.first().map(String::as_str).ok_or("missing command")?;
    let allowed: &[&str] = match verb {
        "keygen" => &["--out"],
        "server" => &["--bind", "--edge", "--key-file", "--image-clipboard"],
        "client" => &["--server", "--key-file", "--image-clipboard"],
        "gui" => &[],
        _ => return Err(format!("unknown command: {verb}")),
    };
    let mut seen = std::collections::HashSet::new();
    let mut index = 1;
    while index < args.len() {
        let key = args[index].as_str();
        if !allowed.contains(&key) || !seen.insert(key) {
            return Err(format!("unknown or duplicate option: {key}"));
        }
        if args
            .get(index + 1)
            .is_none_or(|v| v.is_empty() || v.starts_with("--"))
        {
            return Err(format!("{key} requires a value"));
        }
        index += 2;
    }
    if value_after(args, "--image-clipboard").is_some_and(|v| v != "on" && v != "off") {
        return Err("--image-clipboard must be on or off".into());
    }
    Ok(match verb {
        "keygen" => Command::Keygen {
            output: value_after(args, "--out")
                .map(PathBuf::from)
                .unwrap_or(secure::key_path().map_err(|e| e.to_string())?),
        },
        "server" => Command::Server {
            bind: value_after(args, "--bind").unwrap_or_else(|| "0.0.0.0:24920".into()),
            edge: value_after(args, "--edge")
                .map(|v| server::Edge::parse(&v))
                .transpose()?
                .unwrap_or(server::Edge::Both),
        },
        "client" => Command::Client {
            server: value_after(args, "--server")
                .ok_or("client requires --server WINDOWS_IP:24920")?,
        },
        "gui" => Command::Gui,
        _ => unreachable!(),
    })
}
fn value_after(args: &[String], key: &str) -> Option<String> {
    args.windows(2).find(|p| p[0] == key).map(|p| p[1].clone())
}
fn print_usage() {
    println!(
        "CrossDesk {} - Windows keyboard/mouse -> Apple Silicon Mac
  crossdesk keygen [--out PATH]
  crossdesk server [--bind 0.0.0.0:24920] [--edge both|left|right] [--key-file PATH] [--image-clipboard on|off]
  crossdesk client --server WINDOWS_IP:24920 [--key-file PATH] [--image-clipboard on|off]

Use the same crossdesk.key on both machines. Both horizontal edges are enabled by default.
Emergency release on Windows: Ctrl+Alt+Esc or Scroll Lock.
Image clipboard sync is on by default. Copy screenshots or PNG/JPG files, then paste on the other computer.
Both computers must run protocol v5. Text and arbitrary file sharing stay off by default.",
        env!("CARGO_PKG_VERSION")
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }
    #[test]
    fn image_clipboard_option_accepts_on_off_and_rejects_other_values() {
        assert!(parse_args(&args("server --image-clipboard off")).is_ok());
        assert!(
            parse_args(&args(
                "client --server 127.0.0.1:24920 --image-clipboard on"
            ))
            .is_ok()
        );
        assert!(parse_args(&args("server --image-clipboard yes")).is_err());
        assert!(parse_args(&args("server --image-clipboard")).is_err());
    }
    #[test]
    fn command_line_defaults_to_both_edges_and_rejects_mistyped_flags() {
        assert!(matches!(
            parse_args(&args("server")).unwrap(),
            Command::Server {
                edge: server::Edge::Both,
                ..
            }
        ));
        assert!(parse_args(&args("server --edge")).is_err());
        assert!(parse_args(&args("server --edge top")).is_err());
        assert!(parse_args(&args("client --server --key-file foo")).is_err());
        assert!(parse_args(&args("server --edge both --edge left")).is_err());
        assert!(parse_args(&args("server --edeg both")).is_err());
    }
}
