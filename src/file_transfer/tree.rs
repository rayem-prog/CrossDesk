// CrossDesk folder clipboard: 2026-10-22 (GPL-3.0-only).
//! Bounded source inventory. No file handles survive scanning.
use super::wire::{Entry, MAX_DEPTH, MAX_NODES, Offer};
use super::{MAX_BATCH_BYTES, MAX_FILE_BYTES, MAX_FILES, invalid, safe_name};
use std::{
    fs::{self, File, Metadata, OpenOptions},
    io,
    path::{Path, PathBuf},
};

pub fn cancelled(cancel: &mut impl FnMut() -> bool) -> io::Result<()> {
    if cancel() {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "copy superseded or transfer stopped",
        ))
    } else {
        Ok(())
    }
}
pub fn local(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() || !(metadata.is_dir() || metadata.is_file()) {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & (0x400 | 0x1000 | 0x40000 | 0x400000) != 0 {
            return false;
        }
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt;
        // SF_DATALESS: do not hydrate a cloud placeholder while copying.
        if metadata.st_flags() & 0x40000000 != 0 {
            return false;
        }
    }
    true
}
fn unchanged(old: &Metadata, new: &Metadata) -> bool {
    if !local(new)
        || old.is_dir() != new.is_dir()
        || old.len() != new.len()
        || old.modified().ok() != new.modified().ok()
        || old.created().ok() != new.created().ok()
    {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if old.dev() != new.dev()
            || old.ino() != new.ino()
            || old.ctime() != new.ctime()
            || old.ctime_nsec() != new.ctime_nsec()
        {
            return false;
        }
    }
    true
}
fn contextual(path: &Path, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{}: {error}", path.display()))
}
struct SourceNode {
    path: PathBuf,
    metadata: Metadata,
    #[cfg(windows)]
    identity: WindowsIdentity,
}

#[cfg(windows)]
#[derive(PartialEq, Eq)]
struct WindowsIdentity {
    volume: u32,
    index: u64,
}
#[cfg(windows)]
fn windows_identity(file: &File) -> io::Result<WindowsIdentity> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if info.dwFileAttributes & (0x400 | 0x1000 | 0x40000 | 0x400000) != 0 {
        return Err(invalid("source became a link or cloud placeholder"));
    }
    Ok(WindowsIdentity {
        volume: info.dwVolumeSerialNumber,
        index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    })
}
#[cfg(windows)]
fn windows_source(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    };
    OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}
pub struct SourceTree {
    pub entries: Vec<Entry>,
    nodes: Vec<SourceNode>,
    total: u64,
}
impl SourceTree {
    pub fn scan(paths: &[PathBuf], cancel: &mut impl FnMut() -> bool) -> io::Result<Self> {
        if paths.is_empty() || paths.len() > MAX_FILES {
            return Err(invalid("select 1 to 64 files or folders"));
        }
        let mut tree = Self {
            entries: Vec::new(),
            nodes: Vec::new(),
            total: 0,
        };
        for path in paths {
            tree.visit(path, None, 1, cancel)
                .map_err(|e| contextual(path, e))?;
        }
        tree.verify(cancel)?;
        Ok(tree)
    }
    fn visit(
        &mut self,
        path: &Path,
        parent: Option<u16>,
        depth: usize,
        cancel: &mut impl FnMut() -> bool,
    ) -> io::Result<()> {
        cancelled(cancel)?;
        if self.nodes.len() >= MAX_NODES || depth > MAX_DEPTH {
            return Err(invalid("tree exceeds 10,000 entries or 32 levels"));
        }
        let metadata = fs::symlink_metadata(path)?;
        if !local(&metadata) {
            return Err(invalid(
                "links, reparse points, cloud placeholders and special files cannot be copied; download cloud items first",
            ));
        }
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| safe_name(s))
            .ok_or_else(|| invalid("unsupported cross-platform filename"))?
            .to_owned();
        let directory = metadata.is_dir();
        let len = if directory { 0 } else { metadata.len() };
        self.total = self
            .total
            .checked_add(len)
            .ok_or_else(|| invalid("batch size overflow"))?;
        if len > MAX_FILE_BYTES || self.total > MAX_BATCH_BYTES {
            return Err(invalid("file exceeds 512 MiB or batch exceeds 2 GiB"));
        }
        let index = self.nodes.len();
        self.entries.push(Entry {
            parent,
            directory,
            name,
            len,
        });
        self.nodes.push(SourceNode {
            path: path.to_owned(),
            #[cfg(windows)]
            identity: {
                let file = windows_source(path)?;
                if !unchanged(&metadata, &file.metadata()?) {
                    return Err(invalid("source changed while scanning"));
                }
                windows_identity(&file)?
            },
            metadata,
        });
        if directory {
            // Bound enumeration as well as the final inventory. Never collect an
            // unbounded directory listing, including when all entries are invalid.
            let mut children = Vec::new();
            for child in fs::read_dir(path)? {
                cancelled(cancel)?;
                if children.len() + self.nodes.len() >= MAX_NODES {
                    return Err(invalid("tree exceeds 10,000 entries"));
                }
                children.push(child?.path());
            }
            children.sort();
            for child in children {
                self.visit(&child, Some(index as u16), depth + 1, cancel)
                    .map_err(|e| contextual(&child, e))?;
            }
        } else {
            // Check readability now, then close. Sending opens only one file.
            drop(self.open(index)?);
        }
        Ok(())
    }
    fn check(&self, index: usize) -> io::Result<()> {
        let n = &self.nodes[index];
        #[cfg(windows)]
        if windows_identity(&windows_source(&n.path)?)? != n.identity {
            return Err(invalid("source file or directory was replaced; copy again"));
        }
        if !unchanged(
            &n.metadata,
            &fs::symlink_metadata(&n.path).map_err(|e| contextual(&n.path, e))?,
        ) {
            return Err(invalid(&format!(
                "source changed during transfer: {}; copy again",
                n.path.display()
            )));
        }
        Ok(())
    }
    pub fn verify(&self, cancel: &mut impl FnMut() -> bool) -> io::Result<()> {
        for index in 0..self.nodes.len() {
            cancelled(cancel)?;
            self.check(index)?;
        }
        Ok(())
    }
    pub fn open(&self, index: usize) -> io::Result<File> {
        let mut ancestor = Some(index);
        while let Some(i) = ancestor {
            self.check(i)?;
            ancestor = self.entries[i].parent.map(usize::from);
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
            options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            #[cfg(target_os = "macos")]
            options.custom_flags(0x100); // O_NOFOLLOW
            #[cfg(target_os = "linux")]
            options.custom_flags(0x20000);
        }
        let node = &self.nodes[index];
        let file = options
            .open(&node.path)
            .map_err(|e| contextual(&node.path, e))?;
        if !unchanged(&node.metadata, &file.metadata()?) {
            return Err(invalid("source replaced while opening; copy again"));
        }
        #[cfg(windows)]
        if windows_identity(&file)? != node.identity {
            return Err(invalid("source replaced while opening; copy again"));
        }
        Ok(file)
    }
    pub fn verify_open(&self, index: usize, file: &File) -> io::Result<()> {
        self.check(index)?;
        #[cfg(windows)]
        if windows_identity(file)? != self.nodes[index].identity {
            return Err(invalid("source replaced during transfer; copy again"));
        }
        if !unchanged(&self.nodes[index].metadata, &file.metadata()?) {
            return Err(invalid("source file changed during transfer; copy again"));
        }
        Ok(())
    }
    pub fn offer(&self, id: super::wire::Id) -> Offer {
        Offer {
            id,
            entries: self.entries.clone(),
        }
    }
}

#[cfg(test)]
mod tests;
