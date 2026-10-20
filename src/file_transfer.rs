// CrossDesk ordinary file clipboard changes: 2026-10-20 (GPL-3.0-only).
//! Bounded file sources and exclusive, batch-isolated receive caches.
use crate::protocol::{self, Frame, FrameKind};
use crate::transport::SharedWriter;
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_BUNDLE_SIZE: u64 = MAX_FILE_BYTES; // Legacy single-file guard.
pub const MAX_BATCH_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const MAX_FILES: usize = 64;
pub const MAX_CACHE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub fn safe_name(name: &str) -> bool {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.len() > 255
        || name.ends_with(['.', ' '])
        || name
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
    {
        return false;
    }
    let base = name.split('.').next().unwrap().trim_end().to_uppercase();
    !matches!(
        base.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) && !(base.starts_with("COM") || base.starts_with("LPT"))
        .then(|| &base[3..])
        .is_some_and(|n| {
            matches!(
                n,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
}
fn duplicate_name(name: &str, number: usize) -> io::Result<String> {
    let p = Path::new(name);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    let suffix = p
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| format!(".{s}"))
        .unwrap_or_default();
    let value = format!("{stem} ({number}){suffix}");
    if safe_name(&value) {
        Ok(value)
    } else {
        Err(invalid(
            "duplicate filename exceeds portable filename limit",
        ))
    }
}
pub fn unique_name(name: &str, used: &mut HashSet<String>) -> io::Result<String> {
    if !safe_name(name) {
        return Err(invalid("unsupported or unsafe filename"));
    }
    for n in 1..=MAX_FILES + 1 {
        let value = if n == 1 {
            name.to_owned()
        } else {
            duplicate_name(name, n)?
        };
        if used.insert(value.to_lowercase()) {
            return Ok(value);
        }
    }
    Err(invalid("too many colliding filenames"))
}
fn ordinary(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Includes symbolic links, junctions, cloud placeholders and offline files.
        if metadata.file_attributes() & (0x400 | 0x1000 | 0x40000 | 0x400000) != 0 {
            return false;
        }
    }
    metadata.file_type().is_file()
}
pub struct Source {
    pub name: String,
    pub len: u64,
    pub file: File,
    modified: Option<SystemTime>,
}
impl Source {
    pub fn unchanged(&self) -> io::Result<()> {
        let m = self.file.metadata()?;
        if m.len() != self.len || m.modified().ok() != self.modified {
            return Err(invalid("source file changed during transfer; copy again"));
        }
        Ok(())
    }
}
pub fn sources(files: &[PathBuf]) -> io::Result<Vec<Source>> {
    if files.is_empty() || files.len() > MAX_FILES {
        return Err(invalid("select 1 to 64 ordinary files"));
    }
    let mut used = HashSet::new();
    let mut total = 0u64;
    let mut out = Vec::new();
    for path in files {
        let m = fs::symlink_metadata(path)?;
        if !ordinary(&m) || m.len() > MAX_FILE_BYTES {
            return Err(invalid(
                "only local ordinary files up to 512 MiB are supported; directories and links are skipped",
            ));
        }
        total = total
            .checked_add(m.len())
            .ok_or_else(|| invalid("file batch size overflow"))?;
        if total > MAX_BATCH_BYTES {
            return Err(invalid("file batch exceeds 2 GiB"));
        }
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| invalid("filename is not UTF-8"))?;
        let file = File::open(path)?;
        let opened = file.metadata()?;
        if !ordinary(&opened)
            || opened.len() != m.len()
            || opened.modified().ok() != m.modified().ok()
        {
            return Err(invalid("source changed while opening"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if m.ino() != opened.ino() || m.dev() != opened.dev() {
                return Err(invalid("source replaced while opening"));
            }
        }
        out.push(Source {
            name: unique_name(name, &mut used)?,
            len: m.len(),
            modified: m.modified().ok(),
            file,
        });
    }
    Ok(out)
}
/// Retained only for explicitly opted-in legacy callers. v6 clipboard uses session::FileSender.
pub fn send_files(writer: &SharedWriter, files: &[PathBuf]) -> io::Result<()> {
    for mut source in sources(files)? {
        writer.write(Frame::new(
            FrameKind::FileStart,
            protocol::encode_file_start(&source.name, source.len),
        ))?;
        let mut remaining = source.len;
        let mut buffer = vec![0; protocol::CHUNK_SIZE];
        while remaining > 0 {
            let n = source
                .file
                .read(&mut buffer[..remaining.min(protocol::CHUNK_SIZE as u64) as usize])?;
            if n == 0 {
                return Err(invalid("source file truncated during transfer"));
            }
            writer.write(Frame::new(FrameKind::FileChunk, buffer[..n].to_vec()))?;
            remaining -= n as u64;
        }
        source.unchanged()?;
        writer.write(Frame::new(FrameKind::FileEnd, Vec::new()))?;
    }
    Ok(())
}
pub struct ReceiveFile {
    file: File,
    remaining: u64,
}
impl ReceiveFile {
    pub fn write_chunk(&mut self, chunk: &[u8]) -> io::Result<bool> {
        if chunk.len() as u64 > self.remaining {
            return Err(invalid("file chunk exceeds announced length"));
        }
        self.file.write_all(chunk)?;
        self.remaining -= chunk.len() as u64;
        Ok(self.remaining == 0)
    }
}
pub struct IncomingBundle {
    root: PathBuf,
    directory: Option<PathBuf>,
    current: Option<ReceiveFile>,
    files: Vec<PathBuf>,
    total: u64,
    used: HashSet<String>,
}
impl IncomingBundle {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            directory: None,
            current: None,
            files: Vec::new(),
            total: 0,
            used: HashSet::new(),
        }
    }
    pub fn start_file(&mut self, name: &str, len: u64) -> io::Result<()> {
        let result = self.start_inner(name, len);
        if result.is_err() {
            self.abort();
        }
        result
    }
    fn start_inner(&mut self, name: &str, len: u64) -> io::Result<()> {
        if self.current.as_ref().is_some_and(|f| f.remaining != 0) {
            return Err(invalid("previous file is incomplete"));
        }
        if !safe_name(name)
            || len > MAX_FILE_BYTES
            || self.files.len() >= MAX_FILES
            || self.total + len > MAX_BATCH_BYTES
        {
            return Err(invalid("file manifest exceeds name, size or count limits"));
        }
        if self.directory.is_none() {
            fs::create_dir_all(&self.root)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700))?;
            }
            let mut id = [0; 16];
            getrandom::getrandom(&mut id).map_err(|e| io::Error::other(e.to_string()))?;
            let path = self
                .root
                .join(id.iter().map(|b| format!("{b:02x}")).collect::<String>());
            fs::create_dir(&path)?;
            self.directory = Some(path);
        }
        let name = unique_name(name, &mut self.used)?;
        let directory = self.directory.as_ref().unwrap();
        let mut candidate = name.clone();
        for n in 1..=MAX_FILES + 1 {
            let path = directory.join(&candidate);
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(file) => {
                    self.current = Some(ReceiveFile {
                        file,
                        remaining: len,
                    });
                    self.files.push(path);
                    self.total += len;
                    return Ok(());
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    candidate = duplicate_name(&name, n + 1)?;
                }
                Err(e) => return Err(e),
            }
        }
        Err(invalid("cannot allocate a distinct destination filename"))
    }
    pub fn write_chunk(&mut self, chunk: &[u8]) -> io::Result<()> {
        let result = match self.current.as_mut() {
            Some(file) => file.write_chunk(chunk).map(|_| ()),
            None => Err(invalid("file data without a manifest")),
        };
        if result.is_err() {
            self.abort();
        }
        result
    }
    pub fn finish(&mut self) -> io::Result<Vec<PathBuf>> {
        if self.files.is_empty() || self.current.as_ref().is_some_and(|f| f.remaining != 0) {
            self.abort();
            return Err(invalid("incomplete or empty file batch"));
        }
        self.current = None;
        self.directory = None;
        self.total = 0;
        self.used.clear();
        Ok(std::mem::take(&mut self.files))
    }
    pub fn abort(&mut self) {
        self.current = None;
        if let Some(path) = self.directory.take() {
            let _ = fs::remove_dir_all(path);
        }
        self.files.clear();
        self.used.clear();
        self.total = 0;
    }
}
impl Drop for IncomingBundle {
    fn drop(&mut self) {
        self.abort();
    }
}

#[cfg(test)]
mod review_regressions {
    use super::*;
    fn root(label: &str) -> PathBuf {
        let mut id = [0; 8];
        getrandom::getrandom(&mut id).unwrap();
        std::env::temp_dir().join(format!(
            "crossdesk-review-{label}-{:x}",
            u64::from_le_bytes(id)
        ))
    }
    #[test]
    fn review_truncated_file_must_not_be_published() {
        let root = root("truncated");
        let mut incoming = IncomingBundle::new(root.clone());
        incoming.start_file("report.pdf", 10).unwrap();
        incoming.write_chunk(b"abc").unwrap();
        let result = incoming.finish();
        drop(incoming);
        if root.exists() {
            std::fs::remove_dir_all(root).unwrap();
        }
        assert!(result.is_err(), "finish must reject 3 of 10 declared bytes");
    }

    #[test]
    fn review_next_batch_must_preserve_previously_published_files() {
        let root = root("lifetime");
        let mut incoming = IncomingBundle::new(root.clone());
        incoming.start_file("old.pdf", 3).unwrap();
        incoming.write_chunk(b"old").unwrap();
        let published = incoming.finish().unwrap();
        assert!(published[0].exists());
        incoming.start_file("next.pdf", 3).unwrap();
        let old_still_exists = published[0].exists();
        drop(incoming);
        std::fs::remove_dir_all(root).unwrap();
        assert!(
            old_still_exists,
            "starting the next batch deletes a previously published clipboard file"
        );
    }
    #[test]
    fn review_duplicate_basenames_must_preserve_both_files() {
        let root = root("duplicates");
        let mut incoming = IncomingBundle::new(root.clone());
        incoming.start_file("report.pdf", 5).unwrap();
        incoming.write_chunk(b"first").unwrap();
        incoming.start_file("report.pdf", 6).unwrap();
        incoming.write_chunk(b"second").unwrap();
        let published = incoming.finish().unwrap();
        let contents: Vec<_> = published
            .iter()
            .map(|p| std::fs::read(p).unwrap())
            .collect();
        drop(incoming);
        std::fs::remove_dir_all(root).unwrap();
        assert_eq!(
            contents.len(),
            2,
            "two selected files with the same basename became one file; contents={contents:?}"
        );
    }
    #[test]
    fn review_receiver_must_reject_an_oversized_manifest() {
        let root = root("limit");
        let mut incoming = IncomingBundle::new(root.clone());
        let accepted = incoming
            .start_file("oversized.pdf", MAX_BUNDLE_SIZE + 1)
            .is_ok();
        drop(incoming);
        if root.exists() {
            std::fs::remove_dir_all(root).unwrap();
        }
        assert!(
            !accepted,
            "the receiving side does not enforce even the sender's 512 MiB limit"
        );
    }
}

#[cfg(test)]
mod tests;

pub mod session;
pub mod wire;
