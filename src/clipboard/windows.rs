// CrossDesk image clipboard changes: 2026-10-20 (GPL-3.0-only).
use std::io::Cursor;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use std::{ptr, slice};

use windows_sys::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, POINT};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardOwner,
    GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GMEM_ZEROINIT, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::System::Ole::{
    CF_DIB, CF_HDROP, CF_LOCALE, CF_OEMTEXT, CF_TEXT, CF_UNICODETEXT,
};
use windows_sys::Win32::UI::Shell::{DROPFILES, DragQueryFileW, HDROP};

use crate::clipboard::ClipboardApi;
use crate::protocol::ClipboardPayload;

const BMP_FILE_HEADER_LEN: usize = 14;
const BI_RGB: u32 = 0;
const DROPEFFECT_COPY: u32 = 1;
const TEXT_ORIGIN_FORMAT: &str = "org.crossdesk.clipboard.text-origin.v1";

// A real owner is required by EmptyClipboard / SetClipboardData. A NULL
// OpenClipboard owner is valid for reads but cannot publish clipboard data.
pub struct Clipboard {
    owner: usize,
}

impl Clipboard {
    pub fn new() -> std::io::Result<Self> {
        Ok(Self { owner: 0 })
    }

    fn owner_window(&mut self) -> std::io::Result<windows_sys::Win32::Foundation::HWND> {
        use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, HWND_MESSAGE};
        if self.owner == 0 {
            let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
            let owner = unsafe {
                CreateWindowExW(
                    0,
                    class.as_ptr(),
                    ptr::null(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    HWND_MESSAGE,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null(),
                )
            };
            if owner.is_null() {
                return Err(last_os_error("cannot create clipboard owner window"));
            }
            self.owner = owner as usize;
        }
        Ok(self.owner as _)
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        if self.owner != 0 {
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::DestroyWindow(self.owner as _);
            }
        }
    }
}

impl ClipboardApi for Clipboard {
    fn publish_revision_if_current(
        &mut self,
        payload: &ClipboardPayload,
        expected: u64,
    ) -> std::io::Result<Option<u64>> {
        self.publish(payload, Some(expected))
    }
    fn change_id(&self) -> std::io::Result<u64> {
        Ok(unsafe { GetClipboardSequenceNumber() } as u64)
    }
    fn read(&mut self) -> std::io::Result<Option<ClipboardPayload>> {
        let _guard = ClipboardGuard::open(ptr::null_mut())?;
        if unsafe { IsClipboardFormatAvailable(CF_HDROP as u32) } != 0 {
            if let Some(files) = read_files()? {
                return Ok(Some(ClipboardPayload::Files(files)));
            }
        }
        if let Some(format) = registered_clipboard_format("PNG") {
            if unsafe { IsClipboardFormatAvailable(format) } != 0 {
                let handle = unsafe { GetClipboardData(format) };
                if !handle.is_null() {
                    let data = copy_global(handle as HGLOBAL)?;
                    crate::image_transfer::decode_image(&data)?;
                    return Ok(Some(ClipboardPayload::Bitmap(data)));
                }
            }
        }
        if unsafe { IsClipboardFormatAvailable(CF_DIB as u32) } != 0 {
            if let Some(bitmap) = read_bitmap()? {
                return Ok(Some(ClipboardPayload::Bitmap(bitmap)));
            }
        }
        if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT as u32) } != 0 {
            if let Some(text) = read_text()? {
                return Ok(Some(ClipboardPayload::Text(text)));
            }
        }
        if unsafe { IsClipboardFormatAvailable(CF_TEXT as u32) } != 0 {
            if let Some(text) = read_narrow_text(CF_TEXT as u32)? {
                return Ok(Some(ClipboardPayload::Text(text)));
            }
        }
        if unsafe { IsClipboardFormatAvailable(CF_OEMTEXT as u32) } != 0 {
            if let Some(text) = read_narrow_text(CF_OEMTEXT as u32)? {
                return Ok(Some(ClipboardPayload::Text(text)));
            }
        }
        Ok(None)
    }

    fn write(&mut self, payload: &ClipboardPayload) -> std::io::Result<()> {
        self.publish(payload, None).map(|_| ())
    }
    fn write_if_current(
        &mut self,
        payload: &ClipboardPayload,
        expected: u64,
    ) -> std::io::Result<bool> {
        self.publish(payload, Some(expected))
            .map(|revision| revision.is_some())
    }
}

impl Clipboard {
    fn publish(
        &mut self,
        payload: &ClipboardPayload,
        expected: Option<u64>,
    ) -> std::io::Result<Option<u64>> {
        if let ClipboardPayload::Bitmap(data) = payload {
            crate::image_transfer::decode_image(data)?;
        }
        // Validate and encode text before EmptyClipboard can affect the destination.
        let prepared_text = match payload {
            ClipboardPayload::Text(text) => {
                let normalized = crate::text_transfer::normalize(text)?;
                let bytes = encode_text(&normalized)?;
                let mut origin = [0; 16];
                getrandom::getrandom(&mut origin)
                    .map_err(|_| invalid_data("text publication identity unavailable"))?;
                let format = registered_clipboard_format(TEXT_ORIGIN_FORMAT)
                    .ok_or_else(|| last_os_error("cannot register text publication format"))?;
                Some((bytes, normalized, origin, format))
            }
            _ => None,
        };
        // Precompute the optional bitmap before opening the shared system clipboard.
        let extra = match payload {
            ClipboardPayload::Files(files) if files.len() == 1 => {
                crate::clipboard::image_file_bytes(&files[0])?
            }
            _ => None,
        };
        let owner = self.owner_window()?;
        let guard = ClipboardGuard::open(owner)?;
        if expected
            .is_some_and(|revision| unsafe { GetClipboardSequenceNumber() } as u64 != revision)
        {
            return Ok(None);
        }
        if unsafe { EmptyClipboard() } == 0 {
            return Err(last_os_error("EmptyClipboard failed"));
        }
        let result = match payload {
            ClipboardPayload::Text(_) => {
                let (bytes, _, origin, format) = prepared_text.as_ref().expect("text was prepared");
                set_clipboard_bytes(CF_UNICODETEXT as u32, bytes)?;
                set_clipboard_bytes(*format, origin)
            }
            ClipboardPayload::Bitmap(bitmap) => write_bitmap(bitmap),
            ClipboardPayload::Files(files) => {
                write_files(files)?;
                if let Some(bytes) = &extra {
                    let _ = write_bitmap(bytes);
                }
                Ok(())
            }
        };
        result?;
        if let Some((_, normalized, origin, format)) = prepared_text {
            // Closing adds Windows' synthesized text formats and can advance the
            // sequence. Reopen and prove ownership before attributing that revision.
            drop(guard);
            return finalize_text_receipt(owner, format, &origin, &normalized);
        }
        // Non-text publications keep their receipt under the original lock.
        Ok(Some(unsafe { GetClipboardSequenceNumber() } as u64))
    }
}

fn finalize_text_receipt(
    owner: windows_sys::Win32::Foundation::HWND,
    format: u32,
    origin: &[u8; 16],
    expected_text: &str,
) -> std::io::Result<Option<u64>> {
    let _guard = ClipboardGuard::open(ptr::null_mut())?;
    if unsafe { GetClipboardOwner() } != owner {
        return Ok(None);
    }
    let handle = unsafe { GetClipboardData(format) };
    if handle.is_null() || copy_global_prefix(handle as HGLOBAL, origin.len())? != origin {
        return Ok(None);
    }
    if read_text()?.as_deref() != Some(expected_text) {
        return Ok(None);
    }
    // Force every synthesized text representation before sampling the receipt.
    // Otherwise a later paste/read could advance the revision and echo our text.
    for format in [CF_TEXT, CF_OEMTEXT, CF_LOCALE] {
        if unsafe { GetClipboardData(format as u32) }.is_null() {
            return Err(last_os_error("cannot materialize clipboard text format"));
        }
    }
    Ok(Some(unsafe { GetClipboardSequenceNumber() } as u64))
}

struct ClipboardGuard;

impl ClipboardGuard {
    fn open(owner: windows_sys::Win32::Foundation::HWND) -> std::io::Result<Self> {
        for _ in 0..8 {
            if unsafe { OpenClipboard(owner) } != 0 {
                return Ok(Self);
            }
            thread::sleep(Duration::from_millis(12));
        }
        Err(last_os_error("OpenClipboard failed"))
    }
}

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        unsafe {
            CloseClipboard();
        }
    }
}

fn read_text() -> std::io::Result<Option<String>> {
    let handle = unsafe { GetClipboardData(CF_UNICODETEXT as u32) };
    if handle.is_null() {
        return Ok(None);
    }
    // GlobalSize includes allocator slack. Text length is determined by a NUL
    // within this bounded prefix, never by the size of the allocation.
    let bytes = copy_global_prefix(
        handle as HGLOBAL,
        (crate::text_transfer::MAX_TEXT_BYTES * 2 + 1) * 2,
    )?;
    decode_text(&bytes).map(Some)
}

fn decode_text(bytes: &[u8]) -> std::io::Result<String> {
    // Only complete words preceding the terminator belong to CF_UNICODETEXT;
    // GlobalSize may include an odd number of padding bytes after it.
    let mut words = Vec::with_capacity(bytes.len() / 2);
    let mut terminated = false;
    for chunk in bytes.chunks_exact(2) {
        let word = u16::from_le_bytes([chunk[0], chunk[1]]);
        if word == 0 {
            terminated = true;
            break;
        }
        words.push(word);
    }
    if !terminated {
        return Err(crate::text_transfer::invalid(
            "unterminated UTF-16 clipboard text",
        ));
    }
    let text = String::from_utf16(&words)
        .map_err(|_| crate::text_transfer::invalid("invalid UTF-16 clipboard text"))?;
    crate::text_transfer::normalize(&text)
}

fn encode_text(text: &str) -> std::io::Result<Vec<u8>> {
    let text = crate::text_transfer::normalize(text)?.replace('\n', "\r\n");
    Ok(text
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect())
}

fn read_narrow_text(format: u32) -> std::io::Result<Option<String>> {
    let handle = unsafe { GetClipboardData(format) };
    if handle.is_null() {
        return Ok(None);
    }
    let bytes = copy_global_bounded(
        handle as HGLOBAL,
        crate::text_transfer::MAX_TEXT_BYTES * 2 + 1,
    )?;
    let text = bytes
        .split(|byte| *byte == 0)
        .next()
        .unwrap_or_default()
        .iter()
        .map(|byte| *byte as char)
        .collect::<String>();
    if text.is_empty() {
        Ok(None)
    } else {
        crate::text_transfer::normalize(&text).map(Some)
    }
}

fn read_bitmap() -> std::io::Result<Option<Vec<u8>>> {
    let handle = unsafe { GetClipboardData(CF_DIB as u32) };
    if handle.is_null() {
        return Ok(None);
    }
    let dib = copy_global(handle as HGLOBAL)?;
    if dib.is_empty() {
        return Ok(None);
    }
    dib_to_bmp(&dib).map(Some)
}

fn write_bitmap(bitmap: &[u8]) -> std::io::Result<()> {
    let dib = image_to_dib(bitmap).or_else(|_| bmp_to_dib(bitmap))?;
    set_clipboard_bytes(CF_DIB as u32, &dib)?;
    if let Ok(png) = image_to_png(bitmap) {
        if let Some(format) = registered_clipboard_format("PNG") {
            let _ = set_clipboard_bytes(format, &png);
        }
    }
    Ok(())
}

fn read_files() -> std::io::Result<Option<Vec<PathBuf>>> {
    let handle = unsafe { GetClipboardData(CF_HDROP as u32) };
    if handle.is_null() {
        return Ok(None);
    }
    let hdrop = handle as HDROP;
    let count = unsafe { DragQueryFileW(hdrop, u32::MAX, ptr::null_mut(), 0) };
    if count == 0 {
        return Ok(None);
    }

    let mut files = Vec::new();
    for i in 0..count {
        let len = unsafe { DragQueryFileW(hdrop, i, ptr::null_mut(), 0) };
        if len == 0 {
            continue;
        }
        let mut buf = vec![0u16; len as usize + 1];
        let written = unsafe { DragQueryFileW(hdrop, i, buf.as_mut_ptr(), buf.len() as u32) };
        if written != 0 {
            files.push(PathBuf::from(String::from_utf16_lossy(
                &buf[..written as usize],
            )));
        }
    }
    if files.is_empty() {
        Ok(None)
    } else {
        Ok(Some(files))
    }
}

fn write_files(files: &[PathBuf]) -> std::io::Result<()> {
    let mut list = Vec::<u16>::new();
    for file in files {
        let path = if file.is_absolute() {
            file.clone()
        } else {
            std::env::current_dir()?.join(file)
        };
        list.extend(path.as_os_str().encode_wide());
        list.push(0);
    }
    list.push(0);

    let header_len = size_of::<DROPFILES>();
    let bytes_len = header_len + list.len() * size_of::<u16>();
    let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, bytes_len) };
    if handle.is_null() {
        return Err(last_os_error("GlobalAlloc failed"));
    }

    let memory = unsafe { GlobalLock(handle) as *mut u8 };
    if memory.is_null() {
        unsafe {
            GlobalFree(handle);
        }
        return Err(last_os_error("GlobalLock failed"));
    }

    unsafe {
        let mut dropfiles: DROPFILES = zeroed();
        dropfiles.pFiles = header_len as u32;
        dropfiles.pt = POINT { x: 0, y: 0 };
        dropfiles.fNC = 0;
        dropfiles.fWide = 1;
        ptr::copy_nonoverlapping(
            &dropfiles as *const DROPFILES as *const u8,
            memory,
            header_len,
        );
        ptr::copy_nonoverlapping(
            list.as_ptr() as *const u8,
            memory.add(header_len),
            list.len() * size_of::<u16>(),
        );
        GlobalUnlock(handle);
    }

    let result = unsafe { SetClipboardData(CF_HDROP as u32, handle as HANDLE) };
    if result.is_null() {
        unsafe {
            GlobalFree(handle);
        }
        Err(last_os_error("SetClipboardData failed"))
    } else {
        if let Some(format) = registered_clipboard_format("Preferred DropEffect") {
            let _ = set_clipboard_bytes(format, &DROPEFFECT_COPY.to_le_bytes());
        }
        Ok(())
    }
}

fn copy_global(handle: HGLOBAL) -> std::io::Result<Vec<u8>> {
    copy_global_bounded(handle, crate::image_transfer::MAX_IMAGE_BYTES)
}

fn copy_global_bounded(handle: HGLOBAL, max_bytes: usize) -> std::io::Result<Vec<u8>> {
    let len = unsafe { GlobalSize(handle) };
    if len == 0 {
        return Ok(Vec::new());
    }
    if len > max_bytes {
        return Err(invalid_data("clipboard item exceeds native size bound"));
    }
    let ptr = unsafe { GlobalLock(handle) as *const u8 };
    if ptr.is_null() {
        return Err(last_os_error("GlobalLock failed"));
    }
    let bytes = unsafe { slice::from_raw_parts(ptr, len).to_vec() };
    unsafe {
        GlobalUnlock(handle);
    }
    Ok(bytes)
}

fn copy_global_prefix(handle: HGLOBAL, max_bytes: usize) -> std::io::Result<Vec<u8>> {
    let len = unsafe { GlobalSize(handle) }.min(max_bytes);
    if len == 0 {
        return Ok(Vec::new());
    }
    let memory = unsafe { GlobalLock(handle) as *const u8 };
    if memory.is_null() {
        return Err(last_os_error("GlobalLock failed"));
    }
    let bytes = unsafe { slice::from_raw_parts(memory, len).to_vec() };
    unsafe {
        GlobalUnlock(handle);
    }
    Ok(bytes)
}

fn set_clipboard_bytes(format: u32, bytes: &[u8]) -> std::io::Result<()> {
    let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) };
    if handle.is_null() {
        return Err(last_os_error("GlobalAlloc failed"));
    }
    let memory = unsafe { GlobalLock(handle) as *mut u8 };
    if memory.is_null() {
        unsafe {
            GlobalFree(handle);
        }
        return Err(last_os_error("GlobalLock failed"));
    }
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), memory, bytes.len());
        GlobalUnlock(handle);
    }
    let result = unsafe { SetClipboardData(format, handle as HANDLE) };
    if result.is_null() {
        unsafe {
            GlobalFree(handle);
        }
        Err(last_os_error("SetClipboardData failed"))
    } else {
        Ok(())
    }
}

fn dib_to_bmp(dib: &[u8]) -> std::io::Result<Vec<u8>> {
    let pixel_offset = dib_pixel_offset(dib)? + BMP_FILE_HEADER_LEN;
    let file_size = BMP_FILE_HEADER_LEN + dib.len();
    let mut bmp = Vec::with_capacity(file_size);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&(file_size as u32).to_le_bytes());
    bmp.extend_from_slice(&[0, 0, 0, 0]);
    bmp.extend_from_slice(&(pixel_offset as u32).to_le_bytes());
    bmp.extend_from_slice(dib);
    Ok(bmp)
}

fn bmp_to_dib(bitmap: &[u8]) -> std::io::Result<Vec<u8>> {
    if bitmap.len() >= BMP_FILE_HEADER_LEN && &bitmap[..2] == b"BM" {
        Ok(bitmap[BMP_FILE_HEADER_LEN..].to_vec())
    } else {
        dib_pixel_offset(bitmap)?;
        Ok(bitmap.to_vec())
    }
}

fn image_to_dib(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    let image = crate::image_transfer::decode_image(bytes)?;
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
        return Err(invalid_data("invalid image dimensions"));
    }

    let pixel_len = width as usize * height as usize * 4;
    let mut dib = Vec::with_capacity(40 + pixel_len);
    dib.extend_from_slice(&40u32.to_le_bytes());
    dib.extend_from_slice(&(width as i32).to_le_bytes());
    dib.extend_from_slice(&(height as i32).to_le_bytes());
    dib.extend_from_slice(&1u16.to_le_bytes());
    dib.extend_from_slice(&32u16.to_le_bytes());
    dib.extend_from_slice(&BI_RGB.to_le_bytes());
    dib.extend_from_slice(&(pixel_len as u32).to_le_bytes());
    dib.extend_from_slice(&0i32.to_le_bytes());
    dib.extend_from_slice(&0i32.to_le_bytes());
    dib.extend_from_slice(&0u32.to_le_bytes());
    dib.extend_from_slice(&0u32.to_le_bytes());

    let raw = rgba.as_raw();
    let row_len = width as usize * 4;
    for y in (0..height as usize).rev() {
        let row = &raw[y * row_len..(y + 1) * row_len];
        for pixel in row.chunks_exact(4) {
            dib.push(pixel[2]);
            dib.push(pixel[1]);
            dib.push(pixel[0]);
            dib.push(pixel[3]);
        }
    }
    Ok(dib)
}

fn image_to_png(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok(bytes.to_vec());
    }
    let image = crate::image_transfer::decode_image(bytes)?;
    let mut cursor = Cursor::new(Vec::new());
    image
        .write_to(&mut cursor, image::ImageFormat::Png)
        .map_err(invalid_image)?;
    Ok(cursor.into_inner())
}

fn registered_clipboard_format(name: &str) -> Option<u32> {
    let mut wide = name.encode_utf16().collect::<Vec<_>>();
    wide.push(0);
    let format = unsafe { RegisterClipboardFormatW(wide.as_ptr()) };
    (format != 0).then_some(format)
}

fn dib_pixel_offset(dib: &[u8]) -> std::io::Result<usize> {
    if dib.len() < 4 {
        return Err(invalid_data("short DIB header"));
    }
    let header_len = u32::from_le_bytes(dib[0..4].try_into().unwrap()) as usize;
    if dib.len() < header_len || header_len < 12 {
        return Err(invalid_data("invalid DIB header length"));
    }
    if header_len == 12 {
        return Ok(header_len);
    }
    if dib.len() < 36 {
        return Err(invalid_data("short BITMAPINFOHEADER"));
    }

    let bit_count = u16::from_le_bytes(dib[14..16].try_into().unwrap());
    let compression = u32::from_le_bytes(dib[16..20].try_into().unwrap());
    let clr_used = u32::from_le_bytes(dib[32..36].try_into().unwrap()) as usize;
    let palette_entries = if clr_used != 0 {
        clr_used
    } else if bit_count <= 8 {
        1usize << bit_count
    } else {
        0
    };
    let masks_len = if compression == BI_RGB {
        0
    } else if matches!(compression, 3 | 6) && header_len == 40 {
        12
    } else {
        0
    };
    Ok(header_len + masks_len + palette_entries * 4)
}

fn invalid_data(message: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

fn invalid_image(error: image::ImageError) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error)
}

fn last_os_error(context: &str) -> std::io::Error {
    let error = std::io::Error::last_os_error();
    std::io::Error::new(error.kind(), format!("{context}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_decoder_rejects_invalid_utf16_and_preserves_unicode_with_lf() {
        assert_eq!(decode_text(&[b'x', 0, 0, 0, 0x7f]).unwrap(), "x");
        assert!(decode_text(&[0x00, 0xd8, 0, 0]).is_err());
        assert!(decode_text(&[b'x', 0]).is_err());
        assert!(decode_text(&[0]).is_err());
        assert!(decode_text(&[0, 0]).is_err());
        let bytes = [0x20, 0, 0x2d, 0x4e, 0x0d, 0, 0x0a, 0, 0x09, 0, 0, 0];
        assert_eq!(decode_text(&bytes).unwrap(), " 中\n\t");
        assert_eq!(
            encode_text("a\nb\rc\r\nd").unwrap(),
            [
                97, 0, 13, 0, 10, 0, 98, 0, 13, 0, 10, 0, 99, 0, 13, 0, 10, 0, 100, 0, 0, 0
            ]
        );
    }

    fn png() -> Vec<u8> {
        let image = image::RgbaImage::from_raw(2, 1, vec![255, 0, 0, 255, 0, 255, 0, 127]).unwrap();
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    }
    #[test]
    fn clipboard_owner_is_a_valid_window_and_is_destroyed_with_the_clipboard() {
        use windows_sys::Win32::UI::WindowsAndMessaging::IsWindow;
        let mut clipboard = Clipboard::new().unwrap();
        let owner = clipboard.owner_window().unwrap();
        assert_ne!(unsafe { IsWindow(owner) }, 0);
        drop(clipboard);
        assert_eq!(unsafe { IsWindow(owner) }, 0);
    }
    #[test]
    fn windows_image_representations_have_correct_dimensions_channel_order_and_png_alpha() {
        let bytes = png();
        let dib = image_to_dib(&bytes).unwrap();
        assert_eq!(i32::from_le_bytes(dib[4..8].try_into().unwrap()), 2);
        assert_eq!(i32::from_le_bytes(dib[8..12].try_into().unwrap()), 1);
        assert_eq!(&dib[40..], &[0, 0, 255, 255, 0, 255, 0, 127]);
        let preserved = image_to_png(&bytes).unwrap();
        assert_eq!(
            image::load_from_memory(&preserved)
                .unwrap()
                .into_rgba8()
                .into_raw(),
            vec![255, 0, 0, 255, 0, 255, 0, 127]
        );
    }
    #[test]
    #[ignore = "Uses the Windows system clipboard; run only on the disposable CI desktop"]
    fn native_windows_image_and_file_clipboard_round_trip() {
        assert_eq!(std::env::var("GITHUB_ACTIONS").as_deref(), Ok("true"));
        let mut clipboard = Clipboard::new().unwrap();
        clipboard.write(&ClipboardPayload::Bitmap(png())).unwrap();
        let Some(ClipboardPayload::Bitmap(received)) = clipboard.read().unwrap() else {
            panic!("image missing");
        };
        assert_eq!(
            image::load_from_memory(&received)
                .unwrap()
                .into_rgba8()
                .into_raw(),
            vec![255, 0, 0, 255, 0, 255, 0, 127]
        );
        let path = std::env::temp_dir().join(format!("CrossDesk-截图-{}.png", std::process::id()));
        std::fs::write(&path, png()).unwrap();
        clipboard
            .write(&ClipboardPayload::Files(vec![path.clone()]))
            .unwrap();
        assert!(
            matches!(clipboard.read().unwrap(), Some(ClipboardPayload::Files(files)) if files == vec![path.clone()])
        );
        assert!(
            clipboard
                .write(&ClipboardPayload::Bitmap(vec![1, 2, 3]))
                .is_err()
        );
        assert!(
            matches!(clipboard.read().unwrap(), Some(ClipboardPayload::Files(files)) if files == vec![path.clone()])
        );
        let pdf = std::env::temp_dir().join(format!("CrossDesk-报告-{}.pdf", std::process::id()));
        let empty = pdf.with_extension("empty");
        std::fs::write(&pdf, b"%PDF\0\xff").unwrap();
        std::fs::write(&empty, b"").unwrap();
        let ordinary = ClipboardPayload::Files(vec![pdf.clone(), empty.clone()]);
        let before = clipboard.change_id().unwrap();
        let published = clipboard
            .publish_revision_if_current(&ordinary, before)
            .unwrap()
            .unwrap();
        assert_eq!(clipboard.change_id().unwrap(), published);
        assert_eq!(clipboard.read().unwrap(), Some(ordinary));
        assert!(
            !clipboard
                .write_if_current(&ClipboardPayload::Bitmap(png()), before)
                .unwrap()
        );
        let folder = std::env::temp_dir().join(format!("CrossDesk-folders-{}", std::process::id()));
        let empty_folder = folder.join("空目录 café");
        std::fs::create_dir_all(&empty_folder).unwrap();
        std::fs::write(folder.join(".hidden"), b"nested content").unwrap();
        for paths in [
            vec![empty_folder.clone()],
            vec![folder.clone(), pdf.clone(), empty.clone()],
        ] {
            let selection = ClipboardPayload::Files(paths);
            clipboard.write(&selection).unwrap();
            assert_eq!(clipboard.read().unwrap(), Some(selection));
        }
        assert_eq!(
            std::fs::read(folder.join(".hidden")).unwrap(),
            b"nested content"
        );
        assert_eq!(std::fs::read_dir(&empty_folder).unwrap().count(), 0);
        let text = ClipboardPayload::Text(" \t中文 café 😀\r\nline\rnext\n \t".into());
        let expected_text = ClipboardPayload::Text(" \t中文 café 😀\nline\nnext\n \t".into());
        let before = clipboard.change_id().unwrap();
        let receipt = clipboard
            .publish_revision_if_current(&text, before)
            .unwrap()
            .unwrap();
        assert_eq!(receipt, clipboard.change_id().unwrap());
        assert_eq!(clipboard.read().unwrap(), Some(expected_text.clone()));
        assert_eq!(
            clipboard
                .publish_revision_if_current(&ClipboardPayload::Text("stale".into()), before)
                .unwrap(),
            None
        );
        for invalid in ["".to_owned(), "bad\0text".to_owned(), "x".repeat(1_048_577)] {
            assert!(clipboard.write(&ClipboardPayload::Text(invalid)).is_err());
            assert_eq!(clipboard.change_id().unwrap(), receipt);
            assert_eq!(clipboard.read().unwrap(), Some(expected_text.clone()));
        }
        {
            let _guard = ClipboardGuard::open(ptr::null_mut()).unwrap();
            let handle = unsafe { GetClipboardData(CF_UNICODETEXT as u32) };
            let bytes = copy_global(handle as HGLOBAL).unwrap();
            let wide = bytes
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .take_while(|c| *c != 0)
                .collect::<Vec<_>>();
            assert_eq!(
                String::from_utf16(&wide).unwrap(),
                " \t中文 café 😀\r\nline\r\nnext\r\n \t"
            );
        }
        // A receipt must already include Windows' derived text formats. Reading
        // them later must not look like a fresh native copy to the common worker.
        {
            let _guard = ClipboardGuard::open(ptr::null_mut()).unwrap();
            for format in [CF_TEXT, CF_OEMTEXT, CF_LOCALE] {
                assert!(!unsafe { GetClipboardData(format as u32) }.is_null());
            }
        }
        assert_eq!(clipboard.change_id().unwrap(), receipt);

        // Another application may copy identical text between the initial close
        // and receipt finalization. Matching content alone cannot claim that copy.
        let origin = [93; 16];
        let owner = clipboard.owner_window().unwrap();
        let origin_format = registered_clipboard_format(TEXT_ORIGIN_FORMAT).unwrap();
        {
            let _guard = ClipboardGuard::open(owner).unwrap();
            assert_ne!(unsafe { EmptyClipboard() }, 0);
            set_clipboard_bytes(CF_UNICODETEXT as u32, &encode_text("same text").unwrap()).unwrap();
            set_clipboard_bytes(origin_format, &origin).unwrap();
        }
        // Keep the owner unchanged to specifically verify the missing nonce.
        {
            let _guard = ClipboardGuard::open(owner).unwrap();
            assert_ne!(unsafe { EmptyClipboard() }, 0);
            set_clipboard_bytes(CF_UNICODETEXT as u32, &encode_text("same text").unwrap()).unwrap();
        }
        assert_eq!(
            finalize_text_receipt(owner, origin_format, &origin, "same text").unwrap(),
            None
        );
        assert_eq!(
            clipboard.read().unwrap(),
            Some(ClipboardPayload::Text("same text".into()))
        );

        // Even a clipboard manager preserving registered formats changes the owner.
        let mut other = Clipboard::new().unwrap();
        {
            let _guard = ClipboardGuard::open(other.owner_window().unwrap()).unwrap();
            assert_ne!(unsafe { EmptyClipboard() }, 0);
            set_clipboard_bytes(CF_UNICODETEXT as u32, &encode_text("same text").unwrap()).unwrap();
            set_clipboard_bytes(origin_format, &origin).unwrap();
        }
        assert_eq!(
            finalize_text_receipt(owner, origin_format, &origin, "same text").unwrap(),
            None
        );

        // GlobalSize is an allocation size, not a text length. Ignore allocation
        // slack after a valid bounded NUL, but never search beyond the text cap.
        let bound = (crate::text_transfer::MAX_TEXT_BYTES * 2 + 1) * 2;
        let maximum_newlines = "\n".repeat(crate::text_transfer::MAX_TEXT_BYTES);
        let mut overallocated = encode_text(&maximum_newlines).unwrap();
        overallocated.resize(bound + 4096, 0x7f);
        {
            let _guard = ClipboardGuard::open(owner).unwrap();
            assert_ne!(unsafe { EmptyClipboard() }, 0);
            set_clipboard_bytes(CF_UNICODETEXT as u32, &overallocated).unwrap();
        }
        assert_eq!(
            clipboard.read().unwrap(),
            Some(ClipboardPayload::Text(maximum_newlines))
        );
        let mut unterminated = vec![b'x'; bound + 4096];
        unterminated[bound..bound + 2].copy_from_slice(&[0, 0]);
        {
            let _guard = ClipboardGuard::open(owner).unwrap();
            assert_ne!(unsafe { EmptyClipboard() }, 0);
            set_clipboard_bytes(CF_UNICODETEXT as u32, &unterminated).unwrap();
        }
        assert!(
            clipboard.read().is_err(),
            "terminator beyond native text cap was accepted"
        );

        // Exercise the UTF-8 limit at the native boundary, including the largest
        // UTF-16 allocation after Windows CRLF expansion.
        for maximum in [
            "x".repeat(1_048_576),
            "\n".repeat(1_048_576),
            "🙂".repeat(262_144),
        ] {
            clipboard
                .write(&ClipboardPayload::Text(maximum.clone()))
                .unwrap();
            assert_eq!(
                clipboard.read().unwrap(),
                Some(ClipboardPayload::Text(maximum))
            );
        }
        clipboard
            .write(&ClipboardPayload::Text("local copy".into()))
            .unwrap();
        assert!(clipboard.change_id().unwrap() > receipt);
        assert_eq!(
            clipboard
                .publish_revision_if_current(&text, receipt)
                .unwrap(),
            None
        );
        assert_eq!(
            clipboard.read().unwrap(),
            Some(ClipboardPayload::Text("local copy".into()))
        );
        clipboard
            .write(&ClipboardPayload::Text(" ".into()))
            .unwrap();
        std::fs::remove_file(path).unwrap();
        std::fs::remove_file(pdf).unwrap();
        std::fs::remove_file(empty).unwrap();
        std::fs::remove_dir_all(folder).unwrap();
    }
}
