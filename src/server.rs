#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::request_stop;
#[cfg(windows)]
pub use windows::run;

#[cfg(not(windows))]
pub fn run(_config: ServerConfig) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "This prototype uses Windows as the server; run client mode on the Mac.",
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Both,
}

impl Edge {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            "both" => Ok(Self::Both),
            _ => Err("edge must be left, right, or both".to_string()),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Both => "both",
        }
    }

    pub fn allows(self, side: Self) -> bool {
        self == Self::Both || self == side
    }
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub bind: String,
    pub edge: Edge,
}
