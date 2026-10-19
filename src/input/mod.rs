#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod unsupported;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
pub use linux::InputSink;
#[cfg(target_os = "linux")]
pub use linux::screen_size;
#[cfg(target_os = "macos")]
pub use macos::InputSink;

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
pub use unsupported::InputSink;
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
pub use unsupported::screen_size;
#[cfg(windows)]
pub use windows::InputSink;
