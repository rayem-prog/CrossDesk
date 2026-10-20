// CrossDesk image clipboard changes: 2026-10-20 (GPL-3.0-only).
use std::ffi::{c_char, c_void};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::clipboard::ClipboardApi;
use crate::protocol::ClipboardPayload;

#[link(name = "AppKit", kind = "framework")]
#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> ObjcId;
    fn sel_registerName(name: *const c_char) -> ObjcSel;
    fn objc_msgSend();
}

type ObjcId = *mut c_void;
type ObjcSel = *mut c_void;

pub struct Clipboard;

impl Clipboard {
    pub fn new() -> std::io::Result<Self> {
        Ok(Self)
    }
}

impl ClipboardApi for Clipboard {
    fn change_id(&self) -> std::io::Result<u64> {
        pasteboard_change_count()?
            .map(|v| v as u64)
            .ok_or_else(|| std::io::Error::other("cannot read macOS pasteboard revision"))
    }
    fn read(&mut self) -> std::io::Result<Option<ClipboardPayload>> {
        if let Some(files) = read_files()? {
            return Ok(Some(ClipboardPayload::Files(files)));
        }
        if let Some(bitmap) = read_bitmap()? {
            return Ok(Some(ClipboardPayload::Bitmap(bitmap)));
        }
        if has_file_markers()? {
            return Ok(None);
        }
        if let Some(text) = read_text()? {
            return Ok(Some(ClipboardPayload::Text(text)));
        }
        Ok(None)
    }

    fn write(&mut self, payload: &ClipboardPayload) -> std::io::Result<()> {
        match payload {
            ClipboardPayload::Text(text) => write_text(text),
            ClipboardPayload::Bitmap(bitmap) => write_bitmap(bitmap),
            ClipboardPayload::Files(files) => write_files(files),
        }
    }
    fn publish_revision_if_current(
        &mut self,
        payload: &ClipboardPayload,
        expected: u64,
    ) -> std::io::Result<Option<u64>> {
        let (script, input) = match payload {
            ClipboardPayload::Bitmap(data) => {
                (WRITE_BITMAP_SCRIPT, crate::image_transfer::png_bytes(data)?)
            }
            ClipboardPayload::Files(files) => (WRITE_FILES_SCRIPT, file_input(files)?),
            _ => {
                if self.change_id()? != expected {
                    return Ok(None);
                }
                self.write(payload)?;
                return self.change_id().map(Some);
            }
        };
        let script = format!(
            "{}\n$.NSFileHandle.fileHandleWithStandardOutput.writeData($(String(crossdeskPublicationRevision)).dataUsingEncoding($.NSUTF8StringEncoding));",
            guarded_write_script(script, expected)
        );
        match write_filter("osascript", &["-l", "JavaScript", "-e", &script], &input) {
            Ok(output) => String::from_utf8_lossy(&output)
                .trim()
                .parse::<u64>()
                .map(Some)
                .map_err(|_| std::io::Error::other("missing clipboard publication revision")),
            Err(error)
                if error
                    .to_string()
                    .contains("clipboard changed before publish") =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }
    fn write_if_current(
        &mut self,
        payload: &ClipboardPayload,
        expected: u64,
    ) -> std::io::Result<bool> {
        self.publish_revision_if_current(payload, expected)
            .map(|revision| revision.is_some())
    }
}

fn guarded_write_script(script: &str, expected: u64) -> String {
    script.replace("pasteboard.clearContents;", &format!("if (Number(pasteboard.changeCount) !== {expected}) throw new Error('clipboard changed before publish');\npasteboard.clearContents;"))
}

fn pasteboard_change_count() -> std::io::Result<Option<i64>> {
    unsafe {
        let class = objc_getClass(cstr(b"NSPasteboard\0"));
        if class.is_null() {
            return Ok(None);
        }
        let pasteboard = objc_send_id(class, sel_registerName(cstr(b"generalPasteboard\0")));
        if pasteboard.is_null() {
            return Ok(None);
        }
        let change_count = objc_send_isize(pasteboard, sel_registerName(cstr(b"changeCount\0")));
        Ok(Some(change_count as i64))
    }
}

fn cstr(bytes: &'static [u8]) -> *const c_char {
    bytes.as_ptr().cast()
}

unsafe fn objc_send_id(receiver: ObjcId, selector: ObjcSel) -> ObjcId {
    let send: unsafe extern "C" fn(ObjcId, ObjcSel) -> ObjcId =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { send(receiver, selector) }
}

unsafe fn objc_send_isize(receiver: ObjcId, selector: ObjcSel) -> isize {
    let send: unsafe extern "C" fn(ObjcId, ObjcSel) -> isize =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { send(receiver, selector) }
}

fn read_text() -> std::io::Result<Option<String>> {
    let script = r#"ObjC.import("AppKit");
ObjC.import("Foundation");
function exists(value) { return value !== undefined && value !== null && !(value.isNil && value.isNil()); }
const pasteboard = $.NSPasteboard.generalPasteboard;
for (const type of ["public.utf8-plain-text", "NSStringPboardType", "public.utf16-plain-text"]) {
  const value = pasteboard.stringForType(type);
  if (exists(value)) {
    const text = ObjC.unwrap(value);
    if (text.length > 0) {
      const data = $.NSString.alloc.initWithUTF8String(text).dataUsingEncoding($.NSUTF8StringEncoding);
      $.NSFileHandle.fileHandleWithStandardOutput.writeData(data);
      break;
    }
  }
}
"#;
    let output = Command::new("osascript")
        .args(["-l", "JavaScript", "-e", script])
        .output()?;
    if !output.status.success() || output.stdout.is_empty() {
        return Ok(None);
    }
    let text = String::from_utf8(output.stdout)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(Some(text.replace("\r\n", "\n").replace('\r', "\n")))
}

fn write_text(text: &str) -> std::io::Result<()> {
    let script = r#"ObjC.import("AppKit");
ObjC.import("Foundation");
const data = $.NSFileHandle.fileHandleWithStandardInput.readDataToEndOfFile;
const text = $.NSString.alloc.initWithDataEncoding(data, $.NSUTF8StringEncoding);
if (!text || text.isNil()) throw new Error("invalid UTF-8 text");
const pasteboard = $.NSPasteboard.generalPasteboard;
pasteboard.clearContents;
let wrote = pasteboard.setStringForType(text, "public.utf8-plain-text");
wrote = pasteboard.setStringForType(text, "NSStringPboardType") || wrote;
if (!wrote) throw new Error("set text failed");
"#;
    write_filter(
        "osascript",
        &["-l", "JavaScript", "-e", script],
        text.as_bytes(),
    )
    .map(|_| ())
}

fn read_bitmap() -> std::io::Result<Option<Vec<u8>>> {
    let script = READ_BITMAP_SCRIPT;
    let output = Command::new("osascript")
        .args(["-l", "JavaScript", "-e", script])
        .output()?;
    if output.status.success() && !output.stdout.is_empty() {
        Ok(Some(output.stdout))
    } else {
        Ok(None)
    }
}

fn write_bitmap(bmp: &[u8]) -> std::io::Result<()> {
    let script = WRITE_BITMAP_SCRIPT;
    let png = crate::image_transfer::png_bytes(bmp)?;
    write_filter("osascript", &["-l", "JavaScript", "-e", script], &png).map(|_| ())
}

fn read_files() -> std::io::Result<Option<Vec<PathBuf>>> {
    if let Some(files) = read_files_jxa()? {
        return Ok(Some(files));
    }
    read_files_applescript()
}

fn read_files_jxa() -> std::io::Result<Option<Vec<PathBuf>>> {
    let script = READ_FILES_SCRIPT;
    let output = Command::new("osascript")
        .args(["-l", "JavaScript", "-e", script])
        .output()?;
    if !output.status.success() || output.stdout.is_empty() {
        return Ok(None);
    }
    let files = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    if files.is_empty() {
        Ok(None)
    } else {
        Ok(Some(files))
    }
}

fn read_files_applescript() -> std::io::Result<Option<Vec<PathBuf>>> {
    let script = r#"try
  set clipboardFiles to the clipboard as «class furl»
  if class of clipboardFiles is list then
    repeat with clipboardFile in clipboardFiles
      POSIX path of clipboardFile
    end repeat
  else
    POSIX path of clipboardFiles
  end if
on error
end try
"#;
    let output = Command::new("osascript").args(["-e", script]).output()?;
    if !output.status.success() || output.stdout.is_empty() {
        return Ok(None);
    }
    let files = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    if files.is_empty() {
        Ok(None)
    } else {
        Ok(Some(files))
    }
}

fn has_file_markers() -> std::io::Result<bool> {
    let script = r#"ObjC.import("AppKit");
const pasteboard = $.NSPasteboard.generalPasteboard;
const fileTypes = {
  "public.file-url": true,
  "NSFilenamesPboardType": true,
  "com.apple.finder.noderef": true,
  "com.apple.pasteboard.promised-file-url": true,
  "Apple URL pasteboard type": true
};
const types = pasteboard.types;
let found = false;
for (let i = 0; types && !types.isNil() && i < types.count; i++) {
  if (fileTypes[ObjC.unwrap(types.objectAtIndex(i))]) found = true;
}
const items = pasteboard.pasteboardItems;
for (let i = 0; items && !items.isNil() && i < items.count; i++) {
  const itemTypes = items.objectAtIndex(i).types;
  for (let j = 0; itemTypes && !itemTypes.isNil() && j < itemTypes.count; j++) {
    if (fileTypes[ObjC.unwrap(itemTypes.objectAtIndex(j))]) found = true;
  }
}
if (found) $.NSFileHandle.fileHandleWithStandardOutput.writeData($.NSString.alloc.initWithUTF8String("1").dataUsingEncoding($.NSUTF8StringEncoding));
"#;
    let output = Command::new("osascript")
        .args(["-l", "JavaScript", "-e", script])
        .output()?;
    Ok(output.status.success() && !output.stdout.is_empty())
}

fn file_input(files: &[PathBuf]) -> std::io::Result<Vec<u8>> {
    let image = files.len() == 1 && crate::clipboard::image_file_bytes(&files[0])?.is_some();
    let mut input = if image {
        "image\n".to_string()
    } else {
        "files\n".to_string()
    };
    for file in files {
        let path = file
            .to_str()
            .ok_or_else(|| std::io::Error::other("file path is not UTF-8"))?;
        if path.contains(['\n', '\r', '\0']) {
            return Err(std::io::Error::other(
                "file path contains an unsupported control character",
            ));
        }
        input.push_str(path);
        input.push('\n');
    }
    Ok(input.into_bytes())
}
fn write_files(files: &[PathBuf]) -> std::io::Result<()> {
    write_filter(
        "osascript",
        &["-l", "JavaScript", "-e", WRITE_FILES_SCRIPT],
        &file_input(files)?,
    )
    .map(|_| ())
}

fn write_filter(program: &str, args: &[&str], data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let Some(mut stdin) = child.stdin.take() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "stdin unavailable",
        ));
    };
    stdin.write_all(data)?;
    drop(stdin);
    let output = child.wait_with_output()?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("clipboard command failed: {stderr}"),
        ))
    }
}

const READ_BITMAP_SCRIPT: &str = r#"ObjC.import("AppKit");
ObjC.import("Foundation");
function exists(value) { return value !== undefined && value !== null && !(value.isNil && value.isNil()); }
function pngDataFromImage(image) {
  if (!exists(image) || !image.isValid) return null;
  const tiff = image.TIFFRepresentation;
  if (!exists(tiff)) return null;
  const rep = $.NSBitmapImageRep.imageRepWithData(tiff);
  if (!exists(rep)) return null;
  return rep.representationUsingTypeProperties(4, $({}));
}
function pngDataFromPasteboardData(data) {
  if (!exists(data)) return null;
  const image = $.NSImage.alloc.initWithData(data);
  return pngDataFromImage(image);
}
function hasType(types, expected) {
  for (let i = 0; types && !types.isNil() && i < types.count; i++) {
    if (ObjC.unwrap(types.objectAtIndex(i)) === expected) return true;
  }
  return false;
}
const pasteboard = $.NSPasteboard.generalPasteboard;
const availableTypes = pasteboard.types;
const imageTypes = ["public.png", "public.tiff", "public.jpeg", "public.heic", "com.microsoft.bmp", "public.bmp", "com.apple.pict"];
let out = null;
for (const type of imageTypes) {
  if (!hasType(availableTypes, type)) continue;
  const data = pasteboard.dataForType(type);
  if (exists(data) && data.length > 33554432) throw new Error("image exceeds 32 MiB");
  out = type === "public.png" ? data : pngDataFromPasteboardData(data);
  if (exists(out)) break;
}
if (exists(out)) $.NSFileHandle.fileHandleWithStandardOutput.writeData(out);
"#;

const WRITE_BITMAP_SCRIPT: &str = r#"ObjC.import("AppKit");
ObjC.import("Foundation");
const data = $.NSFileHandle.fileHandleWithStandardInput.readDataToEndOfFile;
const image = $.NSImage.alloc.initWithData(data);
if (!image || image.isNil() || !image.isValid) throw new Error("invalid clipboard image");
const tiff = image.TIFFRepresentation;
const rep = $.NSBitmapImageRep.imageRepWithData(tiff);
if (!rep || rep.isNil()) throw new Error("invalid image representation");
const token = $.NSUUID.UUID.UUIDString;
const item = $.NSPasteboardItem.alloc.init;
if (!item.setDataForType(data,"public.png")) throw new Error("prepare PNG failed");
item.setDataForType(tiff,"public.tiff");
item.setStringForType(token,"org.crossdesk.clipboard-origin");
const pasteboard = $.NSPasteboard.generalPasteboard;
pasteboard.clearContents;
if (!pasteboard.writeObjects($.NSArray.arrayWithObject(item))) throw new Error("set image data failed");
// CrossDesk receipt boundary.
const revision = Number(pasteboard.changeCount);
const origin = pasteboard.stringForType("org.crossdesk.clipboard-origin");
var crossdeskPublicationRevision = origin && !origin.isNil() && ObjC.unwrap(origin) === ObjC.unwrap(token) && Number(pasteboard.changeCount) === revision ? revision : 0;
void 0;
"#;

const READ_FILES_SCRIPT: &str = r#"ObjC.import("AppKit");
ObjC.import("Foundation");
const pasteboard = $.NSPasteboard.generalPasteboard;
const seen = {};
function emitPath(path) {
  if (!path || path.isNil && path.isNil()) return;
  const value = ObjC.unwrap(path);
  if (value && !seen[value]) {
    seen[value] = true;
    const data = $.NSString.alloc.initWithUTF8String(value + "\n").dataUsingEncoding($.NSUTF8StringEncoding);
    $.NSFileHandle.fileHandleWithStandardOutput.writeData(data);
  }
}
function emitUrl(url) {
  if (!url || url.isNil && url.isNil()) return;
  if (!url.isFileURL) return;
  const filePathURL = url.filePathURL;
  if (filePathURL && !filePathURL.isNil()) {
    emitPath(filePathURL.path);
    return;
  }
  emitPath(url.path);
}
function emitFileUrl(value) {
  if (!value || value.isNil && value.isNil()) return;
  const string = ObjC.unwrap(value);
  if (!string) return;
  const url = $.NSURL.URLWithString(string);
  emitUrl(url);
}
emitFileUrl(pasteboard.stringForType("public.file-url"));
emitFileUrl(pasteboard.stringForType("com.apple.pasteboard.promised-file-url"));
const urls = pasteboard.readObjectsForClassesOptions($.NSArray.arrayWithObject($.NSURL.class), $({}));
if (urls && !urls.isNil()) {
  for (let i = 0; i < urls.count; i++) {
    emitUrl(urls.objectAtIndex(i));
  }
}
const legacy = pasteboard.propertyListForType("NSFilenamesPboardType");
if (legacy && !legacy.isNil()) {
  for (let i = 0; i < legacy.count; i++) emitPath(legacy.objectAtIndex(i));
}
const items = pasteboard.pasteboardItems;
if (items && !items.isNil()) {
  for (let i = 0; i < items.count; i++) {
    const item = items.objectAtIndex(i);
    for (const type of ["public.file-url", "com.apple.pasteboard.promised-file-url"]) {
      emitFileUrl(item.stringForType(type));
    }
  }
}
"#;

const WRITE_FILES_SCRIPT: &str = r#"ObjC.import("AppKit");
ObjC.import("Foundation");
const input = $.NSString.alloc.initWithDataEncoding($.NSFileHandle.fileHandleWithStandardInput.readDataToEndOfFile,$.NSUTF8StringEncoding);
const lines = ObjC.unwrap(input).split("\n");
const imageAllowed = lines.shift() === "image";
const paths = lines.filter(Boolean);
const token = $.NSUUID.UUID.UUIDString;
const items = $.NSMutableArray.array;
for (const path of paths) {
  const item = $.NSPasteboardItem.alloc.init;
  const url = $.NSURL.fileURLWithPath(path);
  if (!item.setStringForType(url.absoluteString,"public.file-url")) throw new Error("prepare file URL failed");
  item.setStringForType(url.absoluteString,"NSURLPboardType");
  item.setStringForType(token,"org.crossdesk.clipboard-origin");
  if (imageAllowed && paths.length === 1) {
    const image = $.NSImage.alloc.initWithContentsOfFile(path);
    if (image && !image.isNil() && image.isValid) {
      const tiff = image.TIFFRepresentation;
      const rep = $.NSBitmapImageRep.imageRepWithData(tiff);
      if (rep && !rep.isNil()) {
        const png = rep.representationUsingTypeProperties(4,$({}));
        if (png && !png.isNil()) item.setDataForType(png,"public.png");
        item.setDataForType(tiff,"public.tiff");
      }
    }
  }
  items.addObject(item);
}
const pasteboard = $.NSPasteboard.generalPasteboard;
pasteboard.clearContents;
if (!pasteboard.writeObjects(items)) throw new Error("write file URLs failed");
// CrossDesk receipt boundary.
const revision = Number(pasteboard.changeCount);
const origin = pasteboard.stringForType("org.crossdesk.clipboard-origin");
var crossdeskPublicationRevision = origin && !origin.isNil() && ObjC.unwrap(origin) === ObjC.unwrap(token) && Number(pasteboard.changeCount) === revision ? revision : 0;
void 0;
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn isolated(script: &str, name: &str, input: &[u8]) -> Vec<u8> {
        let script = script.replace(
            "$.NSPasteboard.generalPasteboard",
            &format!("$.NSPasteboard.pasteboardWithName(\"{name}\")"),
        );
        write_filter("osascript", &["-l", "JavaScript", "-e", &script], input).unwrap()
    }
    fn fixture() -> Vec<u8> {
        let pixels =
            image::RgbaImage::from_raw(3, 1, vec![255, 0, 0, 255, 0, 255, 0, 127, 1, 2, 3, 0])
                .unwrap();
        let mut out = Cursor::new(Vec::new());
        pixels.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }
    #[test]
    fn isolated_macos_pasteboard_preserves_png_alpha_and_pixels() {
        let name = format!("org.crossdesk.test.image.{}", std::process::id());
        isolated(WRITE_BITMAP_SCRIPT, &name, &fixture());
        let png = isolated(READ_BITMAP_SCRIPT, &name, &[]);
        let pixels = image::load_from_memory(&png).unwrap().into_rgba8();
        assert_eq!(
            pixels.into_raw(),
            vec![255, 0, 0, 255, 0, 255, 0, 127, 1, 2, 3, 0]
        );
    }
    #[test]
    fn isolated_macos_pasteboard_reads_multiple_real_file_urls_on_stdout() {
        let name = format!("org.crossdesk.test.files.{}", std::process::id());
        let directory =
            std::env::temp_dir().join(format!("crossdesk-mac-file-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let a = directory.join("第一张.png");
        let b = directory.join("第二张.png");
        std::fs::write(&a, fixture()).unwrap();
        std::fs::write(&b, fixture()).unwrap();
        let input = file_input(&[a.clone(), b.clone()]).unwrap();
        isolated(WRITE_FILES_SCRIPT, &name, &input);
        let output = isolated(READ_FILES_SCRIPT, &name, &[]);
        let paths: Vec<_> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(PathBuf::from)
            .collect();
        assert_eq!(paths, vec![a, b]);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn isolated_macos_single_png_file_also_pastes_as_exact_image_content() {
        let name = format!("org.crossdesk.test.single-file.{}", std::process::id());
        let path =
            std::env::temp_dir().join(format!("crossdesk-single-{}.png", std::process::id()));
        std::fs::write(&path, fixture()).unwrap();
        isolated(
            WRITE_FILES_SCRIPT,
            &name,
            &file_input(&[path.clone()]).unwrap(),
        );
        let output = isolated(READ_FILES_SCRIPT, &name, &[]);
        assert_eq!(
            String::from_utf8(output).unwrap().trim(),
            path.to_str().unwrap()
        );
        let png = isolated(READ_BITMAP_SCRIPT, &name, &[]);
        assert_eq!(
            image::load_from_memory(&png)
                .unwrap()
                .into_rgba8()
                .into_raw(),
            vec![255, 0, 0, 255, 0, 255, 0, 127, 1, 2, 3, 0]
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn isolated_macos_stale_publication_leaves_existing_image_intact() {
        let name = format!("org.crossdesk.test.stale.{}", std::process::id());
        isolated(WRITE_BITMAP_SCRIPT, &name, &fixture());
        let script = guarded_write_script(WRITE_BITMAP_SCRIPT, u64::MAX).replace(
            "$.NSPasteboard.generalPasteboard",
            &format!("$.NSPasteboard.pasteboardWithName(\"{name}\")"),
        );
        let error = write_filter(
            "osascript",
            &["-l", "JavaScript", "-e", &script],
            &fixture(),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("clipboard changed before publish")
        );
        let png = isolated(READ_BITMAP_SCRIPT, &name, &[]);
        assert_eq!(png, fixture());
    }
}

#[cfg(test)]
mod ordinary_file_tests {
    use super::*;
    fn isolated(script: &str, name: &str, input: &[u8]) -> Vec<u8> {
        let script = script.replace(
            "$.NSPasteboard.generalPasteboard",
            &format!("$.NSPasteboard.pasteboardWithName(\"{name}\")"),
        );
        write_filter("osascript", &["-l", "JavaScript", "-e", &script], input).unwrap()
    }
    #[test]
    fn named_pasteboard_publishes_pdf_zip_and_empty_file_without_a_bitmap() {
        let name = format!("org.crossdesk.ordinary.{}", std::process::id());
        let root =
            std::env::temp_dir().join(format!("crossdesk-native-documents-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let files = vec![
            root.join("报告-café.pdf"),
            root.join("archive.zip"),
            root.join("empty"),
        ];
        for (p, data) in files.iter().zip([b"%PDF\0".as_slice(), b"PK\x03\x04", b""]) {
            std::fs::write(p, data).unwrap();
        }
        isolated(WRITE_FILES_SCRIPT, &name, &file_input(&files).unwrap());
        let got = isolated(READ_FILES_SCRIPT, &name, &[]);
        let got: Vec<_> = String::from_utf8(got)
            .unwrap()
            .lines()
            .map(PathBuf::from)
            .collect();
        assert_eq!(got.len(), 3);
        assert_eq!(std::fs::read(&got[0]).unwrap(), b"%PDF\0");
        assert_eq!(std::fs::read(&got[1]).unwrap(), b"PK\x03\x04");
        assert_eq!(std::fs::metadata(&got[2]).unwrap().len(), 0);
        assert!(crate::clipboard::same_payload(
            &ClipboardPayload::Files(got),
            &ClipboardPayload::Files(files)
        ));
        assert!(isolated(READ_BITMAP_SCRIPT, &name, &[]).is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn named_pasteboard_publication_receipt_precedes_a_subsequent_local_copy() {
        let name = format!("org.crossdesk.receipt.{}", std::process::id());
        let count_script = "ObjC.import('AppKit'); ObjC.import('Foundation'); $.NSFileHandle.fileHandleWithStandardOutput.writeData($(String(Number($.NSPasteboard.generalPasteboard.changeCount))).dataUsingEncoding($.NSUTF8StringEncoding));";
        let before = String::from_utf8(isolated(count_script, &name, &[]))
            .unwrap()
            .parse::<u64>()
            .unwrap();
        let receipt_script = count_script.replace(
            "Number($.NSPasteboard.generalPasteboard.changeCount)",
            "crossdeskPublicationRevision",
        );
        let script = format!(
            "{}\n{}",
            guarded_write_script(WRITE_BITMAP_SCRIPT, before),
            receipt_script
        );
        let pixels = image::RgbaImage::from_pixel(1, 1, image::Rgba([1, 2, 3, 127]));
        let mut data = std::io::Cursor::new(Vec::new());
        pixels.write_to(&mut data, image::ImageFormat::Png).unwrap();
        let receipt = String::from_utf8(isolated(&script, &name, data.get_ref()))
            .unwrap()
            .parse::<u64>()
            .unwrap();
        assert!(receipt > before);
        isolated(WRITE_BITMAP_SCRIPT, &name, data.get_ref());
        let current = String::from_utf8(isolated(count_script, &name, &[]))
            .unwrap()
            .parse::<u64>()
            .unwrap();
        assert!(current > receipt);
    }
    #[test]
    fn identical_local_copy_during_publication_does_not_acquire_remote_origin() {
        let name = format!("org.crossdesk.same-copy.{}", std::process::id());
        let pixels = image::RgbaImage::from_pixel(1, 1, image::Rgba([4, 5, 6, 127]));
        let mut data = std::io::Cursor::new(Vec::new());
        pixels.write_to(&mut data, image::ImageFormat::Png).unwrap();
        // Inject a normal application copy (identical PNG, no CrossDesk marker)
        // after native publication but before its receipt is collected.
        let script=WRITE_BITMAP_SCRIPT.replace("// CrossDesk receipt boundary.","pasteboard.clearContents; pasteboard.setDataForType(data,'public.png');\n// CrossDesk receipt boundary.");
        let script = format!(
            "{}\n$.NSFileHandle.fileHandleWithStandardOutput.writeData($(String(crossdeskPublicationRevision)).dataUsingEncoding($.NSUTF8StringEncoding));",
            script
        );
        let receipt = String::from_utf8(isolated(&script, &name, data.get_ref())).unwrap();
        assert_eq!(receipt, "0");
        assert_eq!(isolated(READ_BITMAP_SCRIPT, &name, &[]), *data.get_ref());
    }
}
