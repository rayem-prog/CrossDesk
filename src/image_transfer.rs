//! Bounded image clipboard transfer over the existing encrypted transport.

use crate::protocol::{CHUNK_SIZE, ClipboardPayload, Frame, FrameKind};
use image::{DynamicImage, ImageFormat, ImageReader, Limits};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::{self, Cursor, Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

pub const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_BATCH_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_IMAGES: usize = 32;
const MAX_PIXELS: u64 = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Bitmap = 1,
    Files = 2,
}

#[derive(Debug)]
pub struct ImageData {
    pub name: String,
    pub data: Vec<u8>,
}

#[derive(Debug)]
pub struct Transfer {
    pub id: [u8; 16],
    pub kind: Kind,
    pub images: Vec<ImageData>,
}

pub fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub fn decode_image(data: &[u8]) -> io::Result<DynamicImage> {
    if data.is_empty() || data.len() > MAX_IMAGE_BYTES {
        return Err(invalid("image exceeds 32 MiB limit or is empty"));
    }
    let mut reader = ImageReader::new(Cursor::new(data)).with_guessed_format()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits.clone());
    let (width, height) = reader.into_dimensions().map_err(invalid_image)?;
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(invalid("image exceeds 32 Mi pixel limit"));
    }
    let mut reader = ImageReader::new(Cursor::new(data)).with_guessed_format()?;
    reader.limits(limits);
    reader.decode().map_err(invalid_image)
}

fn invalid_image(error: image::ImageError) -> io::Error {
    invalid(format!("invalid image: {error}"))
}

pub fn png_bytes(data: &[u8]) -> io::Result<Vec<u8>> {
    let image = decode_image(data)?;
    let mut png = Cursor::new(Vec::new());
    image
        .write_to(&mut png, ImageFormat::Png)
        .map_err(invalid_image)?;
    let bytes = png.into_inner();
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(invalid("normalized PNG exceeds 32 MiB"));
    }
    Ok(bytes)
}

pub fn safe_image_name(name: &str) -> bool {
    if name.is_empty()
        || name.len() > 255
        || name.ends_with(['.', ' '])
        || name
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
    {
        return false;
    }
    let base = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (base.len() == 4
            && (base.starts_with("COM") || base.starts_with("LPT"))
            && matches!(base.as_bytes()[3], b'1'..=b'9'))
    {
        return false;
    }
    Path::new(name)
        .extension()
        .and_then(|v| v.to_str())
        .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg"))
}

impl Transfer {
    pub fn from_clipboard(payload: &ClipboardPayload) -> io::Result<Self> {
        let mut id = [0; 16];
        getrandom::getrandom(&mut id).map_err(|e| io::Error::other(e.to_string()))?;
        let (kind, images) = match payload {
            ClipboardPayload::Bitmap(bytes) => (
                Kind::Bitmap,
                vec![ImageData {
                    name: "clipboard.png".into(),
                    data: png_bytes(bytes)?,
                }],
            ),
            ClipboardPayload::Files(files) => {
                if files.is_empty() || files.len() > MAX_IMAGES {
                    return Err(invalid("select 1 to 32 image files"));
                }
                let mut images = Vec::new();
                let mut used = HashSet::new();
                let mut total = 0;
                for path in files {
                    let metadata = std::fs::symlink_metadata(path)?;
                    if !metadata.file_type().is_file() || metadata.len() > MAX_IMAGE_BYTES as u64 {
                        return Err(invalid(
                            "only ordinary PNG/JPG files up to 32 MiB are supported",
                        ));
                    }
                    let name = path
                        .file_name()
                        .and_then(|v| v.to_str())
                        .ok_or_else(|| invalid("invalid image filename"))?;
                    if !safe_image_name(name) {
                        return Err(invalid(
                            "selection contains unsupported or unsafe filenames",
                        ));
                    }
                    let mut data = Vec::new();
                    std::fs::File::open(path)?
                        .take((MAX_IMAGE_BYTES + 1) as u64)
                        .read_to_end(&mut data)?;
                    validate_file_image(name, &data)?;
                    total += data.len();
                    if total > MAX_BATCH_BYTES {
                        return Err(invalid("image batch exceeds 128 MiB"));
                    }
                    let name = unique_name(name, &mut used)?;
                    images.push(ImageData { name, data });
                }
                (Kind::Files, images)
            }
            _ => return Err(invalid("only image clipboard content is synchronized")),
        };
        Ok(Self { id, kind, images })
    }

    pub fn fingerprint(&self) -> io::Result<[u8; 32]> {
        let mut hash = Sha256::new();
        hash.update([self.kind as u8]);
        for image in &self.images {
            if self.kind == Kind::Bitmap {
                let pixels = decode_image(&image.data)?.into_rgba8();
                hash.update(pixels.width().to_be_bytes());
                hash.update(pixels.height().to_be_bytes());
                hash.update(pixels.as_raw());
            } else {
                hash.update((image.name.len() as u64).to_be_bytes());
                hash.update(image.name.as_bytes());
                hash.update((image.data.len() as u64).to_be_bytes());
                hash.update(&image.data);
            }
        }
        Ok(hash.finalize().into())
    }

    pub fn visit_frames(&self, mut send: impl FnMut(Frame) -> io::Result<()>) -> io::Result<()> {
        if self.images.is_empty() || self.images.len() > MAX_IMAGES {
            return Err(invalid("invalid image count"));
        }
        let mut manifest = self.id.to_vec();
        manifest.push(self.kind as u8);
        manifest.extend_from_slice(&(self.images.len() as u16).to_be_bytes());
        let mut total = 0usize;
        for image in &self.images {
            if !safe_image_name(&image.name)
                || image.data.is_empty()
                || image.data.len() > MAX_IMAGE_BYTES
            {
                return Err(invalid("invalid image entry"));
            }
            total += image.data.len();
            if total > MAX_BATCH_BYTES {
                return Err(invalid("image batch exceeds 128 MiB"));
            }
            manifest.extend_from_slice(&(image.name.len() as u16).to_be_bytes());
            manifest.extend_from_slice(image.name.as_bytes());
            manifest.extend_from_slice(&(image.data.len() as u64).to_be_bytes());
            manifest.extend_from_slice(&Sha256::digest(&image.data));
        }
        send(Frame::new(FrameKind::ImageStart, manifest))?;
        let mut offset = 0u64;
        for image in &self.images {
            for chunk in image.data.chunks(CHUNK_SIZE) {
                let mut bytes = self.id.to_vec();
                bytes.extend_from_slice(&offset.to_be_bytes());
                bytes.extend_from_slice(chunk);
                send(Frame::new(FrameKind::ImageChunk, bytes))?;
                offset += chunk.len() as u64;
            }
        }
        send(Frame::new(FrameKind::ImageEnd, self.id.to_vec()))
    }

    pub fn into_payload(self, root: &Path) -> io::Result<ClipboardPayload> {
        // Decode everything before creating files or changing a native clipboard.
        for image in &self.images {
            validate_file_image(&image.name, &image.data)?;
        }
        if self.kind == Kind::Bitmap {
            if self.images.len() != 1 {
                return Err(invalid("bitmap transfer requires one image"));
            }
            return Ok(ClipboardPayload::Bitmap(
                self.images.into_iter().next().unwrap().data,
            ));
        }
        std::fs::create_dir_all(root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))?;
        }
        let name: String = self.id.iter().map(|b| format!("{b:02x}")).collect();
        let directory = root.join(name);
        std::fs::create_dir(&directory)?;
        let result = (|| {
            let mut files = Vec::new();
            for image in self.images {
                let path = directory.join(image.name);
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                options.open(&path)?.write_all(&image.data)?;
                files.push(path);
            }
            Ok(ClipboardPayload::Files(files))
        })();
        if result.is_err() {
            let _ = std::fs::remove_dir_all(directory);
        }
        result
    }
}

fn validate_file_image(name: &str, data: &[u8]) -> io::Result<()> {
    if !safe_image_name(name) {
        return Err(invalid("unsafe or unsupported image filename"));
    }
    let format = image::guess_format(data).map_err(invalid_image)?;
    if !matches!(format, ImageFormat::Png | ImageFormat::Jpeg) {
        return Err(invalid("image file content must be PNG or JPEG"));
    }
    decode_image(data).map(|_| ())
}

fn unique_name(name: &str, used: &mut HashSet<String>) -> io::Result<String> {
    if used.insert(name.to_lowercase()) {
        return Ok(name.to_string());
    }
    let path = Path::new(name);
    let stem = path.file_stem().unwrap().to_str().unwrap();
    let extension = path.extension().unwrap().to_str().unwrap();
    for suffix in 2..=MAX_IMAGES + 1 {
        let candidate = format!("{stem} ({suffix}).{extension}");
        if safe_image_name(&candidate) && used.insert(candidate.to_lowercase()) {
            return Ok(candidate);
        }
    }
    Err(invalid(
        "cannot preserve duplicate image filenames within filename limits",
    ))
}

#[derive(Default)]
pub struct Incoming {
    current: Option<Assembly>,
}
struct Entry {
    name: String,
    len: usize,
    digest: [u8; 32],
}
struct Assembly {
    id: [u8; 16],
    kind: Kind,
    entries: Vec<Entry>,
    total: usize,
    bytes: Vec<u8>,
    progress: Instant,
}

impl Incoming {
    pub fn active(&self) -> bool {
        self.current.is_some()
    }
    pub fn expire(&mut self) {
        if self
            .current
            .as_ref()
            .is_some_and(|a| a.progress.elapsed() > Duration::from_secs(30))
        {
            self.current = None;
        }
    }
    pub fn receive(&mut self, frame: Frame) -> io::Result<Option<Transfer>> {
        let result = self.receive_inner(frame);
        if result.is_err() {
            self.current = None;
        }
        result
    }
    fn receive_inner(&mut self, frame: Frame) -> io::Result<Option<Transfer>> {
        match frame.kind {
            FrameKind::ImageStart => {
                if self.current.is_some() {
                    return Err(invalid("image transfer already in progress"));
                }
                let mut reader = Bytes::new(&frame.payload);
                let id = reader.take(16)?.try_into().unwrap();
                let kind = match reader.take(1)?[0] {
                    1 => Kind::Bitmap,
                    2 => Kind::Files,
                    _ => return Err(invalid("unknown image transfer kind")),
                };
                let count = u16::from_be_bytes(reader.take(2)?.try_into().unwrap()) as usize;
                if count == 0 || count > MAX_IMAGES || (kind == Kind::Bitmap && count != 1) {
                    return Err(invalid("invalid image count"));
                }
                let mut entries = Vec::new();
                let mut total = 0usize;
                let mut names = HashSet::new();
                for _ in 0..count {
                    let length = u16::from_be_bytes(reader.take(2)?.try_into().unwrap()) as usize;
                    if length > 255 {
                        return Err(invalid("image filename too long"));
                    }
                    let name = std::str::from_utf8(reader.take(length)?)
                        .map_err(|_| invalid("filename is not UTF-8"))?
                        .to_string();
                    if !safe_image_name(&name) || !names.insert(name.to_lowercase()) {
                        return Err(invalid("unsafe or duplicate image filename"));
                    }
                    let length = u64::from_be_bytes(reader.take(8)?.try_into().unwrap());
                    if length == 0 || length > MAX_IMAGE_BYTES as u64 {
                        return Err(invalid("image exceeds 32 MiB or is empty"));
                    }
                    total += length as usize;
                    if total > MAX_BATCH_BYTES {
                        return Err(invalid("image batch exceeds 128 MiB"));
                    }
                    let digest = reader.take(32)?.try_into().unwrap();
                    entries.push(Entry {
                        name,
                        len: length as usize,
                        digest,
                    });
                }
                if !reader.rest().is_empty() {
                    return Err(invalid("trailing manifest data"));
                }
                self.current = Some(Assembly {
                    id,
                    kind,
                    entries,
                    total,
                    bytes: Vec::new(),
                    progress: Instant::now(),
                });
                Ok(None)
            }
            FrameKind::ImageChunk => {
                let active = self
                    .current
                    .as_mut()
                    .ok_or_else(|| invalid("image chunk without manifest"))?;
                let mut reader = Bytes::new(&frame.payload);
                if reader.take(16)? != active.id {
                    return Err(invalid("image transfer ID mismatch"));
                }
                let offset = u64::from_be_bytes(reader.take(8)?.try_into().unwrap());
                let data = reader.rest();
                if offset != active.bytes.len() as u64
                    || data.is_empty()
                    || data.len() > CHUNK_SIZE
                    || data.len() > active.total - active.bytes.len()
                {
                    return Err(invalid("invalid image chunk offset or length"));
                }
                active.bytes.extend_from_slice(data);
                active.progress = Instant::now();
                Ok(None)
            }
            FrameKind::ImageEnd => {
                let active = self
                    .current
                    .take()
                    .ok_or_else(|| invalid("image completion without manifest"))?;
                if frame.payload != active.id || active.bytes.len() != active.total {
                    return Err(invalid("incomplete image transfer"));
                }
                let mut images = Vec::new();
                let mut offset = 0;
                for entry in active.entries {
                    let data = &active.bytes[offset..offset + entry.len];
                    if Sha256::digest(data).as_slice() != entry.digest {
                        return Err(invalid("image content digest mismatch"));
                    }
                    images.push(ImageData {
                        name: entry.name,
                        data: data.to_vec(),
                    });
                    offset += entry.len;
                }
                Ok(Some(Transfer {
                    id: active.id,
                    kind: active.kind,
                    images,
                }))
            }
            _ => Err(invalid("unexpected image frame")),
        }
    }
}

struct Bytes<'a> {
    data: &'a [u8],
    offset: usize,
}
impl<'a> Bytes<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }
    fn take(&mut self, length: usize) -> io::Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| invalid("image metadata overflow"))?;
        let bytes = self
            .data
            .get(self.offset..end)
            .ok_or_else(|| invalid("truncated image metadata"))?;
        self.offset = end;
        Ok(bytes)
    }
    fn rest(&self) -> &'a [u8] {
        &self.data[self.offset..]
    }
}

#[cfg(test)]
mod tests;
