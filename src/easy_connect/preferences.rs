use super::*;
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Preferences {
    pub auto_connect: bool,
    pub image_clipboard: bool,
    pub file_clipboard: bool,
    pub last_address: Option<SocketAddr>,
    pub peer_name: String,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            auto_connect: true,
            image_clipboard: true,
            file_clipboard: true,
            last_address: None,
            peer_name: String::new(),
        }
    }
}
impl Preferences {
    pub fn load(store: &TrustedStore) -> io::Result<Self> {
        let path = store.root().join("preferences.json");
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e),
            Ok(m) if !m.is_file() || m.file_type().is_symlink() || m.len() > 16384 => {
                return Err(io::Error::other("设置文件不正确"));
            }
            _ => {}
        }
        serde_json::from_slice(&std::fs::read(path)?)
            .map_err(|_| io::Error::other("设置文件损坏，请检查应用数据目录"))
    }
    pub fn save(&self, store: &TrustedStore) -> io::Result<()> {
        let mut n = [0; 8];
        getrandom::getrandom(&mut n).map_err(|_| io::Error::other("无法保存设置"))?;
        let tmp = store.root().join(format!(".pref-{}.tmp", hex(&n)));
        let target = store.root().join("preferences.json");
        let result = (|| {
            use std::io::Write;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&tmp)?;
            file.write_all(&serde_json::to_vec(self).map_err(io::Error::other)?)?;
            file.sync_all()?;
            drop(file);
            #[cfg(not(windows))]
            {
                std::fs::rename(&tmp, &target)?;
            }
            #[cfg(windows)]
            {
                use std::os::windows::ffi::OsStrExt;
                use windows_sys::Win32::Storage::FileSystem::{
                    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
                };
                let from: Vec<_> = tmp.as_os_str().encode_wide().chain(Some(0)).collect();
                let to: Vec<_> = target.as_os_str().encode_wide().chain(Some(0)).collect();
                if unsafe {
                    MoveFileExW(
                        from.as_ptr(),
                        to.as_ptr(),
                        MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                    )
                } == 0
                {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        })();
        let _ = std::fs::remove_file(tmp);
        result
    }
}
