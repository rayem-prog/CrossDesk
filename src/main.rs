#![allow(dead_code)]

mod client;
mod clipboard;
mod clipboard_sync;
mod config;
mod control;
mod file_transfer;
mod gui;
mod input;
#[cfg(target_os = "linux")]
mod linux;
mod platform;
mod pointer;
mod protocol;
mod server;
mod shutdown;
mod transport;

use std::env;
use std::process::ExitCode;

#[derive(Debug)]
enum Command {
    Gui,
    Server { bind: String, edge: server::Edge },
    Client { server: String },
}

fn main() -> ExitCode {
    let cli_args = env::args().skip(1).collect::<Vec<_>>();
    let help_requested = cli_args
        .iter()
        .any(|arg| matches!(arg.as_str(), "-h" | "--help"));
    if help_requested {
        print_usage();
        return ExitCode::SUCCESS;
    }

    let command = match parse_args(&cli_args) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("{error}");
            print_usage();
            return ExitCode::FAILURE;
        }
    };

    let result = match command {
        Command::Gui => {
            #[cfg(target_os = "linux")]
            linux::prepare_gui_environment();
            gui::run()
        }
        Command::Server { bind, edge } => server::run(server::ServerConfig { bind, edge }),
        Command::Client { server } => client::run(&server),
    };
    if let Err(error) = result {
        eprintln!("error: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn parse_args(args: &[String]) -> Result<Command, String> {
    match args.first().map(String::as_str) {
        None | Some("gui") => Ok(Command::Gui),
        Some("server") => {
            let bind =
                option_value_after(args, "--bind").unwrap_or_else(|| "0.0.0.0:24920".to_string());
            let edge = option_value_after(args, "--edge")
                .map(|value| server::Edge::parse(&value))
                .transpose()?
                .unwrap_or(server::Edge::Right);
            Ok(Command::Server { bind, edge })
        }
        Some("client") => {
            let server = option_value_after(args, "--server")
                .ok_or_else(|| "client mode requires --server HOST:PORT".to_string())?;
            Ok(Command::Client { server })
        }
        _ => Err("missing command: server or client".to_string()),
    }
}

fn option_value_after(args: &[String], key: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == key)
        .map(|pair| pair[1].clone())
}

fn print_usage() {
    eprintln!(
        "Usage:
  deskbridge server --bind 0.0.0.0:24920
  deskbridge server --bind 0.0.0.0:24920 --edge right
  deskbridge client --server WINDOWS_IP:24920
  deskbridge gui

Goal:
  Windows/macOS keyboard and mouse sharing, text/image/file clipboard,
  low-latency input, and GUI file transfer."
    );
}
