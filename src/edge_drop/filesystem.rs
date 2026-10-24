use std::{io, path::Path};
#[cfg(unix)]
fn cpath(p: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(p.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
}
/// OS-enforced no-replace rename; checking exists before rename is racy.
pub(super) fn rename_exclusive(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn renamex_np(
                from: *const std::ffi::c_char,
                to: *const std::ffi::c_char,
                flags: u32,
            ) -> i32;
        }
        let a = cpath(from)?;
        let b = cpath(to)?;
        if unsafe { renamex_np(a.as_ptr(), b.as_ptr(), 4) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(target_os = "linux")]
    {
        let a = cpath(from)?;
        let b = cpath(to)?;
        if unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                a.as_ptr(),
                libc::AT_FDCWD,
                b.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        } == 0
        {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
        let a = crate::file_transfer::windows_native_path(from)?;
        let b = crate::file_transfer::windows_native_path(to)?;
        // In particular, do not request MOVEFILE_REPLACE_EXISTING.
        if unsafe { MoveFileExW(a.as_ptr(), b.as_ptr(), 0) } != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (from, to);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "exclusive rename unavailable",
        ))
    }
}
pub(super) fn available_space(path: &Path) -> io::Result<u64> {
    #[cfg(target_os = "macos")]
    {
        // Darwin statvfs uses unsigned long for size/flags and u64 block counts.
        #[repr(C)]
        struct Stat {
            bsize: usize,
            frsize: usize,
            blocks: u64,
            bfree: u64,
            bavail: u64,
            files: u64,
            ffree: u64,
            favail: u64,
            fsid: usize,
            flag: usize,
            namemax: usize,
        }
        unsafe extern "C" {
            fn statvfs(path: *const std::ffi::c_char, s: *mut Stat) -> i32;
        }
        let p = cpath(path)?;
        let mut s: Stat = unsafe { std::mem::zeroed() };
        if unsafe { statvfs(p.as_ptr(), &mut s) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(s.bavail.saturating_mul(s.frsize as u64))
    }
    #[cfg(target_os = "linux")]
    {
        let p = cpath(path)?;
        let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(p.as_ptr(), &mut s) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((s.f_bavail as u64).saturating_mul(s.f_frsize as u64))
    }
    #[cfg(windows)]
    {
        let existing = std::fs::canonicalize(path)?;
        let p = crate::file_transfer::windows_native_path(&existing)?;
        let mut free = 0;
        if unsafe {
            windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
                p.as_ptr(),
                &mut free,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(free)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "disk space check unavailable",
        ))
    }
}
