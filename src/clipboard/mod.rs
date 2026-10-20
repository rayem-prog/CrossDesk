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
    /// Returns the native revision of this publication, before releasing ownership.
    fn publish_revision_if_current(
        &mut self,
        payload: &ClipboardPayload,
        expected: u64,
    ) -> std::io::Result<Option<u64>> {
        if self.write_if_current(payload, expected)? {
            Ok(Some(self.change_id()?))
        } else {
            Ok(None)
        }
    }
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

/// Optional image representation for one file. Never load arbitrary large documents into RAM.
pub fn image_file_bytes(path: &std::path::Path) -> std::io::Result<Option<Vec<u8>>> {
    use std::io::Read;
    if !path
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| matches!(s.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg"))
    {
        return Ok(None);
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file()
        || metadata.len() > crate::image_transfer::MAX_IMAGE_BYTES as u64
    {
        return Ok(None);
    }
    let mut data = Vec::new();
    std::fs::File::open(path)?
        .take(crate::image_transfer::MAX_IMAGE_BYTES as u64 + 1)
        .read_to_end(&mut data)?;
    if crate::image_transfer::decode_image(&data).is_err() {
        return Ok(None);
    }
    Ok(Some(data))
}
#[cfg(test)]
mod file_representation_tests {
    use super::*;
    #[test]
    fn only_bounded_valid_image_files_get_an_extra_bitmap_representation() {
        let root =
            std::env::temp_dir().join(format!("crossdesk-representation-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let image = root.join("image.png");
        let pdf = root.join("document.pdf");
        let large = root.join("large.png");
        let mut data = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(1, 1, image::Rgba([1, 2, 3, 127]))
            .write_to(&mut data, image::ImageFormat::Png)
            .unwrap();
        std::fs::write(&image, data.into_inner()).unwrap();
        std::fs::write(&pdf, b"%PDF-not-an-image").unwrap();
        std::fs::File::create(&large)
            .unwrap()
            .set_len(crate::image_transfer::MAX_IMAGE_BYTES as u64 + 1)
            .unwrap();
        assert!(image_file_bytes(&image).unwrap().is_some());
        assert!(image_file_bytes(&pdf).unwrap().is_none());
        assert!(image_file_bytes(&large).unwrap().is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// Native file URLs may normalize Unicode or expand path aliases. Compare the
/// referenced files, not path spelling, while publication revision protects new copies.
pub fn same_payload(left: &ClipboardPayload, right: &ClipboardPayload) -> bool {
    match (left, right) {
        (ClipboardPayload::Files(a), ClipboardPayload::Files(b)) => {
            a.len() == b.len()
                && a.iter().zip(b).all(|(a, b)| {
                    if a == b {
                        return true;
                    }
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        match (std::fs::metadata(a), std::fs::metadata(b)) {
                            (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
                            _ => false,
                        }
                    }
                    #[cfg(not(unix))]
                    {
                        match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
                            (Ok(a), Ok(b)) => a == b,
                            _ => false,
                        }
                    }
                })
        }
        _ => left == right,
    }
}
#[cfg(test)]
mod identity_tests {
    use super::*;
    #[test]
    fn separate_files_with_identical_bytes_are_not_the_same_clipboard_identity() {
        let root =
            std::env::temp_dir().join(format!("crossdesk-file-identity-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let a = root.join("a.pdf");
        let b = root.join("b.pdf");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(&a, b"identical").unwrap();
        std::fs::write(&b, b"identical").unwrap();
        assert!(!same_payload(
            &ClipboardPayload::Files(vec![a.clone()]),
            &ClipboardPayload::Files(vec![b])
        ));
        assert!(same_payload(
            &ClipboardPayload::Files(vec![a.clone()]),
            &ClipboardPayload::Files(vec![root.join("sub/../a.pdf")])
        ));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn unicode_equivalent_urls_refer_to_the_same_file_but_not_a_second_copy() {
        let root = std::env::temp_dir().join(format!(
            "crossdesk-file-identity-unicode-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("café.pdf");
        std::fs::write(&path, b"%PDF").unwrap();
        assert!(same_payload(
            &ClipboardPayload::Files(vec![path]),
            &ClipboardPayload::Files(vec![root.join("cafe\u{301}.pdf")])
        ));
        std::fs::remove_dir_all(root).unwrap();
    }
}
