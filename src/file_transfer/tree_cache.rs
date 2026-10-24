// CrossDesk folder clipboard: 2026-10-22 (GPL-3.0-only).
//! Unpublished staging owns its whole root, including empty directories.
use super::{
    ReceiveFile, duplicate_name, invalid, safe_name,
    tree::local,
    wire::{MAX_NODES, Offer},
};
use std::{
    collections::HashMap,
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
};
use unicode_normalization::UnicodeNormalization;

pub struct TreeCache {
    root: Option<PathBuf>,
    paths: Vec<PathBuf>,
    top: Vec<PathBuf>,
    current: Option<ReceiveFile>,
}
fn key(name: &str) -> String {
    name.nfc()
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .nfc()
        .collect()
}
fn directory(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}
fn create_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}
pub(crate) fn set_hidden(path: &Path, hidden: bool) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ATTRIBUTE_HIDDEN, GetFileAttributesW, INVALID_FILE_ATTRIBUTES, SetFileAttributesW,
        };
        let p: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let attrs = unsafe { GetFileAttributesW(p.as_ptr()) };
        if attrs == INVALID_FILE_ATTRIBUTES {
            return Err(io::Error::last_os_error());
        }
        let attrs = if hidden {
            attrs | FILE_ATTRIBUTE_HIDDEN
        } else {
            attrs & !FILE_ATTRIBUTE_HIDDEN
        };
        if unsafe { SetFileAttributesW(p.as_ptr(), attrs) } == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    #[cfg(not(windows))]
    let _ = (path, hidden);
    Ok(())
}
impl TreeCache {
    pub fn new(cache: &Path, offer: &Offer) -> io::Result<Self> {
        Self::allocate(cache, offer, false)
    }
    /// The destination already exists and belongs to the user: never chmod it.
    pub fn new_drop(cache: &Path, offer: &Offer) -> io::Result<Self> {
        Self::allocate(cache, offer, true)
    }
    fn allocate(cache: &Path, offer: &Offer, drop_lane: bool) -> io::Result<Self> {
        offer.validate()?;
        if !drop_lane {
            fs::create_dir_all(cache)?;
        }
        if !local(&fs::symlink_metadata(cache)?) || !cache.is_dir() {
            return Err(invalid("unsafe private cache directory"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if !drop_lane {
                fs::set_permissions(cache, fs::Permissions::from_mode(0o700))?;
            }
        }
        let mut id = [0; 16];
        getrandom::getrandom(&mut id).map_err(|e| io::Error::other(e.to_string()))?;
        let name = id.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let root = cache.join(if drop_lane {
            format!(".crossdesk-drop-{name}")
        } else {
            name
        });
        directory(&root)?;
        let mut batch = Self {
            root: Some(root.clone()),
            paths: Vec::new(),
            top: Vec::new(),
            current: None,
        };
        if drop_lane {
            set_hidden(&root, true)?;
        }
        let mut used: HashMap<Option<u16>, HashSet<String>> = HashMap::new();
        for entry in &offer.entries {
            let parent = entry
                .parent
                .map(|p| batch.paths[p as usize].clone())
                .unwrap_or_else(|| root.clone());
            let names = used.entry(entry.parent).or_default();
            let mut allocated = None;
            for n in 1..=MAX_NODES + 1 {
                let name = if n == 1 {
                    entry.name.clone()
                } else {
                    duplicate_name(&entry.name, n)?
                };
                if !safe_name(&name) || names.contains(&key(&name)) {
                    continue;
                }
                let path = parent.join(&name);
                let result = if entry.directory {
                    directory(&path)
                } else {
                    create_file(&path).map(drop)
                };
                match result {
                    Ok(()) => {
                        names.insert(key(&name));
                        allocated = Some(path);
                        break;
                    }
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(e) => return Err(e),
                }
            }
            let path =
                allocated.ok_or_else(|| invalid("cannot allocate distinct destination name"))?;
            if entry.parent.is_none() {
                batch.top.push(path.clone());
            }
            batch.paths.push(path);
        }
        Ok(batch)
    }
    pub fn start(&mut self, index: usize, len: u64) -> io::Result<()> {
        if self.current.as_ref().is_some_and(|f| f.remaining != 0) {
            return Err(invalid("previous file incomplete"));
        }
        self.current = None;
        let file = OpenOptions::new().write(true).open(&self.paths[index])?;
        self.current = Some(ReceiveFile {
            file,
            remaining: len,
        });
        Ok(())
    }
    pub fn write(&mut self, data: &[u8]) -> io::Result<()> {
        self.current
            .as_mut()
            .ok_or_else(|| invalid("file data without file"))?
            .write_chunk(data)
            .map(|_| ())
    }
    pub fn seal(&mut self) -> io::Result<()> {
        let file = self
            .current
            .take()
            .ok_or_else(|| invalid("seal without file"))?;
        if file.remaining != 0 {
            return Err(invalid("incomplete file"));
        }
        file.file.sync_all()?;
        Ok(())
    }
    pub fn finish(mut self) -> io::Result<(PathBuf, Vec<PathBuf>)> {
        if self.current.is_some() {
            return Err(invalid("unsealed file"));
        }
        Ok((self.root.take().unwrap(), std::mem::take(&mut self.top)))
    }
}
impl Drop for TreeCache {
    fn drop(&mut self) {
        self.current = None;
        if let Some(root) = self.root.take() {
            let _ = fs::remove_dir_all(root);
        }
    }
}
pub fn cache_size(root: &Path) -> io::Result<u64> {
    let mut pending = vec![root.to_owned()];
    let mut total = 0u64;
    while let Some(path) = pending.pop() {
        let m = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound && path == root => continue,
            Err(e) => return Err(e),
        };
        if !local(&m) {
            return Err(invalid(
                "unexpected link or placeholder in private file cache",
            ));
        }
        if m.is_dir() {
            for entry in fs::read_dir(path)? {
                pending.push(entry?.path());
            }
        } else {
            total = total
                .checked_add(m.len())
                .ok_or_else(|| invalid("cache size overflow"))?;
        }
    }
    Ok(total)
}
