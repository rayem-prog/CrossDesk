const PLATFORM_UNKNOWN: u8 = 0;
const PLATFORM_WINDOWS: u8 = 1;
const PLATFORM_MACOS: u8 = 2;
const PLATFORM_LINUX: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Platform {
    Unknown = PLATFORM_UNKNOWN,
    Windows = PLATFORM_WINDOWS,
    MacOS = PLATFORM_MACOS,
    Linux = PLATFORM_LINUX,
}

impl Platform {
    pub fn local() -> Self {
        #[cfg(windows)]
        {
            Self::Windows
        }
        #[cfg(target_os = "macos")]
        {
            Self::MacOS
        }
        #[cfg(target_os = "linux")]
        {
            Self::Linux
        }
        #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
        {
            Self::Unknown
        }
    }

    pub fn from_byte(value: u8) -> Self {
        match value {
            PLATFORM_WINDOWS => Self::Windows,
            PLATFORM_MACOS => Self::MacOS,
            PLATFORM_LINUX => Self::Linux,
            _ => Self::Unknown,
        }
    }

    pub fn as_byte(self) -> u8 {
        self as u8
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::MacOS => "macos",
            Self::Linux => "linux",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionProfile {
    WindowsToWindows,
    WindowsToMacOS,
    WindowsToLinux,
    MacOSToWindows,
    MacOSToMacOS,
    MacOSToLinux,
    LinuxToWindows,
    LinuxToMacOS,
    LinuxToLinux,
    Unknown,
}

impl ConnectionProfile {
    pub fn new(server: Platform, client: Platform) -> Self {
        match (server, client) {
            (Platform::Windows, Platform::Windows) => Self::WindowsToWindows,
            (Platform::Windows, Platform::MacOS) => Self::WindowsToMacOS,
            (Platform::Windows, Platform::Linux) => Self::WindowsToLinux,
            (Platform::MacOS, Platform::Windows) => Self::MacOSToWindows,
            (Platform::MacOS, Platform::MacOS) => Self::MacOSToMacOS,
            (Platform::MacOS, Platform::Linux) => Self::MacOSToLinux,
            (Platform::Linux, Platform::Windows) => Self::LinuxToWindows,
            (Platform::Linux, Platform::MacOS) => Self::LinuxToMacOS,
            (Platform::Linux, Platform::Linux) => Self::LinuxToLinux,
            _ => Self::Unknown,
        }
    }

    pub fn local_client(server: Platform) -> Self {
        Self::new(server, Platform::local())
    }

    pub fn local_server(client: Platform) -> Self {
        Self::new(Platform::local(), client)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::WindowsToWindows => "windows-to-windows",
            Self::WindowsToMacOS => "windows-to-macos",
            Self::WindowsToLinux => "windows-to-linux",
            Self::MacOSToWindows => "macos-to-windows",
            Self::MacOSToMacOS => "macos-to-macos",
            Self::MacOSToLinux => "macos-to-linux",
            Self::LinuxToWindows => "linux-to-windows",
            Self::LinuxToMacOS => "linux-to-macos",
            Self::LinuxToLinux => "linux-to-linux",
            Self::Unknown => "unknown",
        }
    }

    pub fn is_same_platform(self) -> bool {
        matches!(
            self,
            Self::WindowsToWindows | Self::MacOSToMacOS | Self::LinuxToLinux
        )
    }
}
