// CrossDesk automatic pairing store, 2026-10-21. GPL-3.0-only.
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};
pub type DeviceId = [u8; 16];
pub struct StoredPeer {
    pub id: DeviceId,
    pub key_path: PathBuf,
}
pub struct TrustedStore {
    root: PathBuf,
    lock: Mutex<()>,
}
fn invalid(s: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, s)
}
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub fn parse_id(text: &str) -> io::Result<DeviceId> {
    if text.len() != 32 || !text.is_ascii() {
        return Err(invalid("invalid device identity"));
    }
    let mut id = [0; 16];
    for (i, b) in id.iter_mut().enumerate() {
        *b = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16)
            .map_err(|_| invalid("invalid device identity"))?;
    }
    Ok(id)
}
fn private_dir(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
            return Err(invalid("private storage is not a directory"));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => fs::create_dir_all(path)?,
        Err(e) => return Err(e),
        _ => {}
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
fn checked_file(path: &Path) -> io::Result<()> {
    let m = fs::symlink_metadata(path)?;
    if !m.is_file() || m.file_type().is_symlink() {
        return Err(invalid("private storage is not a regular file"));
    }
    Ok(())
}
fn create_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut nonce = [0; 16];
    getrandom::getrandom(&mut nonce).map_err(|e| io::Error::other(e.to_string()))?;
    let tmp = path.with_file_name(format!(".{}.tmp", hex(&nonce)));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::hard_link(&tmp, path)
    })();
    let _ = fs::remove_file(tmp);
    result
}
impl TrustedStore {
    pub fn new(root: PathBuf) -> io::Result<Self> {
        private_dir(&root)?;
        Ok(Self {
            root,
            lock: Mutex::new(()),
        })
    }
    pub fn forget(&self) -> io::Result<()> {
        let _lock = self
            .lock
            .lock()
            .map_err(|_| io::Error::other("store lock poisoned"))?;
        for name in ["trusted", "pending"] {
            let path = self.root.join(name);
            match fs::symlink_metadata(&path) {
                Ok(m) if m.is_dir() && !m.file_type().is_symlink() => fs::remove_dir_all(path)?,
                Ok(_) => return Err(invalid("invalid private directory")),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        match fs::remove_file(self.host_key_path()) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        Ok(())
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn local_id(&self) -> io::Result<DeviceId> {
        let _lock = self
            .lock
            .lock()
            .map_err(|_| io::Error::other("store lock poisoned"))?;
        let path = self.root.join("device-id");
        if !path.exists() {
            let mut b = [0; 16];
            getrandom::getrandom(&mut b).map_err(|e| io::Error::other(e.to_string()))?;
            match create_atomic(&path, hex(&b).as_bytes()) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
        checked_file(&path)?;
        parse_id(fs::read_to_string(path)?.trim())
    }
    pub fn host_key(&self) -> io::Result<[u8; 32]> {
        let _lock = self
            .lock
            .lock()
            .map_err(|_| io::Error::other("store lock poisoned"))?;
        let path = self.host_key_path();
        if !path.exists() {
            let mut key = [0; 32];
            getrandom::getrandom(&mut key).map_err(|e| io::Error::other(e.to_string()))?;
            match create_atomic(&path, hex(&key).as_bytes()) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
        checked_file(&path)?;
        crate::secure::load_key_from(&path)
    }
    pub fn host_key_path(&self) -> PathBuf {
        self.root.join("host.key")
    }
    fn paired_inner(&self) -> io::Result<Option<StoredPeer>> {
        let dir = self.root.join("trusted");
        match fs::symlink_metadata(&dir) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
            Ok(m) if !m.is_dir() || m.file_type().is_symlink() => {
                return Err(invalid("invalid trust directory"));
            }
            _ => {}
        }
        let idpath = dir.join("peer-id");
        checked_file(&idpath)?;
        let id = parse_id(fs::read_to_string(idpath)?.trim())?;
        let key_path = dir.join("peer.key");
        checked_file(&key_path)?;
        crate::secure::load_key_from(&key_path)?;
        Ok(Some(StoredPeer { id, key_path }))
    }
    fn pending_inner(&self) -> io::Result<Option<StoredPeer>> {
        let directory = self.root.join("pending");
        match fs::symlink_metadata(&directory) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
            Ok(m) if !m.is_dir() || m.file_type().is_symlink() => {
                return Err(invalid("invalid pending directory"));
            }
            _ => {}
        }
        let mut entries = fs::read_dir(&directory)?;
        let Some(entry) = entries.next().transpose()? else {
            return Ok(None);
        };
        if entries.next().is_some() {
            return Err(invalid("multiple pending pairings"));
        }
        let id = parse_id(
            entry
                .file_name()
                .to_str()
                .ok_or_else(|| invalid("invalid pending identity"))?,
        )?;
        let path = entry.path();
        let m = fs::symlink_metadata(&path)?;
        if !m.is_dir() || m.file_type().is_symlink() {
            return Err(invalid("invalid pending directory"));
        }
        checked_file(&path.join("peer-id"))?;
        if parse_id(fs::read_to_string(path.join("peer-id"))?.trim())? != id {
            return Err(invalid("pending identity mismatch"));
        }
        let key_path = path.join("peer.key");
        checked_file(&key_path)?;
        crate::secure::load_key_from(&key_path)?;
        Ok(Some(StoredPeer { id, key_path }))
    }
    pub fn pending(&self) -> io::Result<Option<StoredPeer>> {
        let _lock = self
            .lock
            .lock()
            .map_err(|_| io::Error::other("store lock poisoned"))?;
        self.pending_inner()
    }
    pub fn paired(&self) -> io::Result<Option<StoredPeer>> {
        let _lock = self
            .lock
            .lock()
            .map_err(|_| io::Error::other("store lock poisoned"))?;
        self.paired_inner()
    }
    pub fn stage(&self, id: DeviceId, key: &[u8; 32]) -> io::Result<()> {
        let _lock = self
            .lock
            .lock()
            .map_err(|_| io::Error::other("store lock poisoned"))?;
        if let Some(old) = self.paired_inner()? {
            if old.id == id && crate::secure::load_key_from(&old.key_path)? == *key {
                return Ok(());
            }
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "another pairing already exists; forget it before replacing",
            ));
        }
        if let Some(old) = self.pending_inner()? {
            if old.id == id && crate::secure::load_key_from(&old.key_path)? == *key {
                return Ok(());
            }
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "another pairing is pending; forget it before replacing",
            ));
        }
        let pending = self.root.join("pending");
        private_dir(&pending)?;
        let mut random = [0; 16];
        getrandom::getrandom(&mut random).map_err(|e| io::Error::other(e.to_string()))?;
        let temp = self.root.join(format!(".staging-{}", hex(&random)));
        private_dir(&temp)?;
        let result: io::Result<()> = (|| {
            create_atomic(&temp.join("peer-id"), hex(&id).as_bytes())?;
            create_atomic(&temp.join("peer.key"), hex(key).as_bytes())?;
            #[cfg(unix)]
            File::open(&temp)?.sync_all()?;
            fs::rename(&temp, pending.join(hex(&id)))?;
            #[cfg(unix)]
            File::open(&pending)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(temp);
        }
        result?;

        Ok(())
    }
    pub fn commit(&self, id: DeviceId) -> io::Result<()> {
        let _lock = self
            .lock
            .lock()
            .map_err(|_| io::Error::other("store lock poisoned"))?;
        if let Some(old) = self.paired_inner()? {
            return if old.id == id {
                Ok(())
            } else {
                Err(invalid("different trusted device already exists"))
            };
        }
        let source = self.root.join("pending").join(hex(&id));
        private_dir(&source)?;
        checked_file(&source.join("peer-id"))?;
        if parse_id(fs::read_to_string(source.join("peer-id"))?.trim())? != id {
            return Err(invalid("pairing identity mismatch"));
        }
        checked_file(&source.join("peer.key"))?;
        crate::secure::load_key_from(&source.join("peer.key"))?;
        fs::rename(source, self.root.join("trusted"))?;
        #[cfg(unix)]
        File::open(&self.root)?.sync_all()?;
        Ok(())
    }
}
#[cfg(test)]
mod tests;
