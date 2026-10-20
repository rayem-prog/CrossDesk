// CrossDesk image clipboard changes: 2026-10-20 (GPL-3.0-only).
use crate::protocol::ClipboardPayload;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod unsupported;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
pub use linux::Clipboard;
#[cfg(target_os = "macos")]
pub use macos::Clipboard;
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
pub use unsupported::Clipboard;
#[cfg(windows)]
pub use windows::Clipboard;

pub trait ClipboardApi {
    fn change_id(&self) -> std::io::Result<u64> {
        Ok(0)
    }
    fn read(&mut self) -> std::io::Result<Option<ClipboardPayload>>;
    fn write(&mut self, payload: &ClipboardPayload) -> std::io::Result<()>;
    fn write_if_current(
        &mut self,
        payload: &ClipboardPayload,
        expected: u64,
    ) -> std::io::Result<bool> {
        if self.change_id()? != expected {
            return Ok(false);
        }
        self.write(payload)?;
        Ok(true)
    }
}

pub mod session;
