use std::io::Cursor;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use std::{ptr, slice};

use windows_sys::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, POINT};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
    IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GMEM_ZEROINIT, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::System::Ole::{CF_DIB, CF_HDROP, CF_OEMTEXT, CF_TEXT, CF_UNICODETEXT};
use windows_sys::Win32::UI::Shell::{DROPFILES, DragQueryFileW, HDROP};

use crate::clipboard::ClipboardApi;
use crate::protocol::ClipboardPayload;

const BMP_FILE_HEADER_LEN: usize = 14;
const BI_RGB: u32 = 0;
const DROPEFFECT_COPY: u32 = 1;

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
    }
}

impl Clipboard {
    fn publish(
        &mut self,
        payload: &ClipboardPayload,
        expected: Option<u64>,
    ) -> std::io::Result<bool> {
        if let ClipboardPayload::Bitmap(data) = payload {
            crate::image_transfer::decode_image(data)?;
        }
        let owner = self.owner_window()?;
        let _guard = ClipboardGuard::open(owner)?;
        if expected
            .is_some_and(|revision| unsafe { GetClipboardSequenceNumber() } as u64 != revision)
        {
            return Ok(false);
        }
        if unsafe { EmptyClipboard() } == 0 {
            return Err(last_os_error("EmptyClipboard failed"));
        }
        let result = match payload {
            ClipboardPayload::Text(text) => write_text(text),
            ClipboardPayload::Bitmap(bitmap) => write_bitmap(bitmap),
            ClipboardPayload::Files(files) => {
                write_files(files)?;
                if files.len() == 1 {
                    if let Ok(bytes) = std::fs::read(&files[0]) {
                        if crate::image_transfer::decode_image(&bytes).is_ok() {
                            let _ = write_bitmap(&bytes);
                        }
                    }
                }
                Ok(())
            }
        };
        result.map(|_| true)
    }
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
    let bytes = copy_global(handle as HGLOBAL)?;
    let words = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .take_while(|ch| *ch != 0)
        .collect::<Vec<_>>();
    if words.is_empty() {
        Ok(None)
    } else {
        Ok(Some(String::from_utf16_lossy(&words)))
    }
}

fn write_text(text: &str) -> std::io::Result<()> {
    let mut wide = text.encode_utf16().collect::<Vec<_>>();
    wide.push(0);
    let bytes = wide
        .iter()
        .flat_map(|ch| ch.to_le_bytes())
        .collect::<Vec<_>>();
    set_clipboard_bytes(CF_UNICODETEXT as u32, &bytes)?;

    let mut ansi = text
        .chars()
        .map(|ch| if ch.is_ascii() { ch as u8 } else { b'?' })
        .collect::<Vec<_>>();
    ansi.push(0);
    let _ = set_clipboard_bytes(CF_TEXT as u32, &ansi);
    Ok(())
}

fn read_narrow_text(format: u32) -> std::io::Result<Option<String>> {
    let handle = unsafe { GetClipboardData(format) };
    if handle.is_null() {
        return Ok(None);
    }
    let bytes = copy_global(handle as HGLOBAL)?;
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
        Ok(Some(text))
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
    let len = unsafe { GlobalSize(handle) };
    if len == 0 {
        return Ok(Vec::new());
    }
    if len > crate::image_transfer::MAX_IMAGE_BYTES {
        return Err(std::io::Error::other("clipboard item exceeds 32 MiB"));
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
        clipboard.write(&ClipboardPayload::Text("".into())).unwrap();
        std::fs::remove_file(path).unwrap();
    }
}
