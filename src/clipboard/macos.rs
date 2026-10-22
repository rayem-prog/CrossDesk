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
            ClipboardPayload::Text(text) => (
                WRITE_TEXT_SCRIPT,
                crate::text_transfer::normalize(text)?.into_bytes(),
            ),
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
    let script = READ_TEXT_SCRIPT;
    let output = Command::new("osascript")
        .args(["-l", "JavaScript", "-e", script])
        .output()?;
    decode_text_output(output)
}

fn decode_text_output(output: std::process::Output) -> std::io::Result<Option<String>> {
    let invalid = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "text is empty, invalid, contains NUL, or exceeds 1 MiB",
        )
    };
    // The subprocess diagnostic is untrusted and must never appear in events.
    if !output.status.success() {
        return Err(invalid());
    }
    if output.stdout.is_empty() {
        return Ok(None);
    }
    let text = String::from_utf8(output.stdout).map_err(|_| invalid())?;
    crate::text_transfer::normalize(&text)
        .map(Some)
        .map_err(|_| invalid())
}

fn write_text(text: &str) -> std::io::Result<()> {
    let script = WRITE_TEXT_SCRIPT;
    write_filter(
        "osascript",
        &["-l", "JavaScript", "-e", script],
        crate::text_transfer::normalize(text)?.as_bytes(),
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

fn decode_file_paths(bytes: &[u8]) -> std::io::Result<Vec<PathBuf>> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let records = bytes
        .strip_suffix(&[0])
        .ok_or_else(|| std::io::Error::other("unterminated native file paths"))?;
    let text = std::str::from_utf8(records)
        .map_err(|_| std::io::Error::other("native file paths are not UTF-8"))?;
    text.split('\0')
        .map(|path| {
            let path = PathBuf::from(path);
            if !path.is_absolute() {
                return Err(std::io::Error::other("invalid native file path"));
            }
            Ok(path)
        })
        .collect()
}
fn read_files_jxa() -> std::io::Result<Option<Vec<PathBuf>>> {
    let output = Command::new("osascript")
        .args(["-l", "JavaScript", "-e", READ_FILES_SCRIPT])
        .output()?;
    if !output.status.success() || output.stdout.is_empty() {
        return Ok(None);
    }
    let files = decode_file_paths(&output.stdout)?;
    Ok((!files.is_empty()).then_some(files))
}

fn read_files_applescript() -> std::io::Result<Option<Vec<PathBuf>>> {
    let script = r#"try
  set clipboardFiles to the clipboard as «class furl»
  if class of clipboardFiles is not list then set clipboardFiles to {clipboardFiles}
  set outputText to ""
  repeat with clipboardFile in clipboardFiles
    set outputText to outputText & (POSIX path of clipboardFile) & (ASCII character 0)
  end repeat
  return outputText
on error
  return ""
end try
"#;
    let output = Command::new("osascript").args(["-e", script]).output()?;
    if !output.status.success() || output.stdout.is_empty() {
        return Ok(None);
    }
    // osascript appends one output newline, separate from NUL-delimited paths.
    let bytes = output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout);
    let files = decode_file_paths(bytes)?;
    Ok((!files.is_empty()).then_some(files))
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
const separator = $.NSData.alloc.initWithBase64EncodedStringOptions("AA==", 0);
function emitPath(path) {
  if (!path || path.isNil && path.isNil()) return;
  const value = ObjC.unwrap(path);
  if (value && !seen[value]) {
    seen[value] = true;
    const data = $.NSString.alloc.initWithUTF8String(value).dataUsingEncoding($.NSUTF8StringEncoding);
    $.NSFileHandle.fileHandleWithStandardOutput.writeData(data);
    $.NSFileHandle.fileHandleWithStandardOutput.writeData(separator);
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
        let paths = decode_file_paths(&output).unwrap();
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
        assert_eq!(decode_file_paths(&output).unwrap(), vec![path.clone()]);
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
    #[test]
    fn named_pasteboard_keeps_a_control_character_path_intact_for_batch_rejection() {
        let name = format!("org.crossdesk.control-path.{}", std::process::id());
        let root =
            std::env::temp_dir().join(format!("crossdesk-control-path-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("invalid\nname");
        std::fs::write(&path, b"must not become two other paths").unwrap();
        let script = r#"ObjC.import('AppKit'); ObjC.import('Foundation');
const path = $.NSString.alloc.initWithDataEncoding($.NSFileHandle.fileHandleWithStandardInput.readDataToEndOfFile,$.NSUTF8StringEncoding);
const pasteboard = $.NSPasteboard.generalPasteboard;
pasteboard.clearContents;
pasteboard.writeObjects($.NSArray.arrayWithObject($.NSURL.fileURLWithPath(path)));
"#;
        isolated(script, &name, path.to_str().unwrap().as_bytes());
        let bytes = isolated(READ_FILES_SCRIPT, &name, &[]);
        let received = decode_file_paths(&bytes).unwrap();
        assert!(crate::file_transfer::tree::SourceTree::scan(&received, &mut || false).is_err());
        std::fs::remove_dir_all(root).unwrap();
        assert_eq!(
            received,
            vec![path],
            "clipboard transport split one invalid name into different source paths"
        );
    }
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
        let got = decode_file_paths(&got).unwrap();
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
    fn named_pasteboard_round_trips_empty_folder_and_mixed_folder_file_selection() {
        let name = format!("org.crossdesk.folders.{}", std::process::id());
        let root =
            std::env::temp_dir().join(format!("crossdesk-native-folders-{}", std::process::id()));
        let empty = root.join("空目录 café");
        let folder = root.join("资料 folder");
        let document = root.join("报告.pdf");
        std::fs::create_dir_all(&empty).unwrap();
        std::fs::create_dir_all(folder.join("nested/empty")).unwrap();
        std::fs::write(folder.join("nested/.hidden"), b"nested content").unwrap();
        std::fs::write(&document, b"%PDF mixed").unwrap();
        for paths in [
            vec![empty.clone()],
            vec![folder.clone(), document.clone(), empty.clone()],
        ] {
            isolated(WRITE_FILES_SCRIPT, &name, &file_input(&paths).unwrap());
            let received = decode_file_paths(&isolated(READ_FILES_SCRIPT, &name, &[])).unwrap();
            assert_eq!(received.len(), paths.len());
            // NSURL may return decomposed Unicode on macOS. The selected
            // filesystem objects must survive even when path spelling changes.
            assert!(crate::clipboard::same_payload(
                &ClipboardPayload::Files(received.clone()),
                &ClipboardPayload::Files(paths),
            ));
            if received.len() == 1 {
                assert!(received[0].is_dir());
                assert_eq!(std::fs::read_dir(&received[0]).unwrap().count(), 0);
            } else {
                assert!(received[0].join("nested/empty").is_dir());
                assert_eq!(
                    std::fs::read(received[0].join("nested/.hidden")).unwrap(),
                    b"nested content"
                );
                assert_eq!(std::fs::read(&received[1]).unwrap(), b"%PDF mixed");
                assert!(received[2].is_dir());
                assert_eq!(std::fs::read_dir(&received[2]).unwrap().count(), 0);
            }
            assert!(isolated(READ_BITMAP_SCRIPT, &name, &[]).is_empty());
        }
        assert_eq!(
            std::fs::read(folder.join("nested/.hidden")).unwrap(),
            b"nested content"
        );
        assert_eq!(std::fs::read_dir(&empty).unwrap().count(), 0);
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

const READ_TEXT_SCRIPT: &str = r#"ObjC.import("AppKit");
ObjC.import("Foundation");
function exists(value) { return value !== undefined && value !== null && !(value.isNil && value.isNil()); }
const pasteboard = $.NSPasteboard.generalPasteboard;
for (const type of ["public.utf8-plain-text", "NSStringPboardType", "public.utf16-plain-text"]) {
  // Bound native data and NSString before bridging any text into JavaScript.
  const nativeData = pasteboard.dataForType(type);
  if (exists(nativeData) && nativeData.length > 4194306) throw new Error("text exceeds native bound");
  const value = pasteboard.stringForType(type);
  if (!exists(value)) {
    if (exists(nativeData)) throw new Error("invalid native text");
    continue;
  }
  if (value.length > 2097152) throw new Error("text exceeds native bound");
  const data = value.dataUsingEncodingAllowLossyConversion($.NSUTF8StringEncoding, false);
  if (!exists(data) || data.length > 2097152) throw new Error("invalid or oversized UTF-8 text");
  const text = ObjC.unwrap(value).replace(/\r\n/g, "\n").replace(/\r/g, "\n");
  if (text.length === 0 || text.indexOf("\0") !== -1) throw new Error("empty or NUL text");
  const normalized = $(text).dataUsingEncoding($.NSUTF8StringEncoding);
  if (normalized.length > 1048576) throw new Error("text exceeds 1 MiB");
  $.NSFileHandle.fileHandleWithStandardOutput.writeData(normalized);
  break;
}
"#;

const WRITE_TEXT_SCRIPT: &str = r#"ObjC.import("AppKit");
ObjC.import("Foundation");
const data = $.NSFileHandle.fileHandleWithStandardInput.readDataOfLength(2097153);
if (data.length > 2097152) throw new Error("text exceeds native bound");
const decoded = $.NSString.alloc.initWithDataEncoding(data, $.NSUTF8StringEncoding);
if (!decoded || decoded.isNil()) throw new Error("invalid UTF-8 text");
const value = ObjC.unwrap(decoded).replace(/\r\n/g, "\n").replace(/\r/g, "\n");
if (value.length === 0 || value.indexOf("\0") !== -1) throw new Error("empty or NUL text");
const text = $(value);
if (text.lengthOfBytesUsingEncoding($.NSUTF8StringEncoding) > 1048576) throw new Error("text exceeds 1 MiB");
const token = $.NSUUID.UUID.UUIDString;
const item = $.NSPasteboardItem.alloc.init;
if (!item.setStringForType(text, "public.utf8-plain-text")) throw new Error("prepare text failed");
item.setStringForType(text, "NSStringPboardType");
if (!item.setStringForType(token, "org.crossdesk.clipboard-origin")) throw new Error("prepare origin failed");
const pasteboard = $.NSPasteboard.generalPasteboard;
pasteboard.clearContents;
if (!pasteboard.writeObjects($.NSArray.arrayWithObject(item))) throw new Error("set text failed");
// CrossDesk receipt boundary.
const revision = Number(pasteboard.changeCount);
const origin = pasteboard.stringForType("org.crossdesk.clipboard-origin");
var crossdeskPublicationRevision = origin && !origin.isNil() && ObjC.unwrap(origin) === ObjC.unwrap(token) && Number(pasteboard.changeCount) === revision ? revision : 0;
void 0;
"#;

#[cfg(test)]
mod text_tests {
    use super::*;
    fn run(script: &str, name: &str, input: &[u8]) -> std::io::Result<Vec<u8>> {
        let script = script.replace(
            "$.NSPasteboard.generalPasteboard",
            &format!("$.NSPasteboard.pasteboardWithName(\"{name}\")"),
        );
        write_filter("osascript", &["-l", "JavaScript", "-e", &script], input)
    }
    fn name(suffix: &str) -> String {
        format!("org.crossdesk.text.{}.{suffix}", std::process::id())
    }
    fn read_named(name: &str) -> std::io::Result<Option<String>> {
        let script = READ_TEXT_SCRIPT.replace(
            "$.NSPasteboard.generalPasteboard",
            &format!("$.NSPasteboard.pasteboardWithName(\"{name}\")"),
        );
        decode_text_output(
            Command::new("osascript")
                .args(["-l", "JavaScript", "-e", &script])
                .output()
                .unwrap(),
        )
    }
    #[test]
    fn named_invalid_native_text_reports_invalid_data_without_exposing_content() {
        let name = name("read-invalid");
        let raw = r#"ObjC.import("AppKit"); ObjC.import("Foundation");
const data = $.NSFileHandle.fileHandleWithStandardInput.readDataToEndOfFile;
const pasteboard = $.NSPasteboard.generalPasteboard;
pasteboard.clearContents;
pasteboard.setDataForType(data, "public.utf8-plain-text");
"#;
        for bytes in [
            b"private secret\0tail".to_vec(),
            vec![b'x'; 1_048_577],
            vec![0xff],
            vec![],
        ] {
            run(raw, &name, &bytes).unwrap();
            let error = read_named(&name).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
            assert!(!error.to_string().contains("private secret"));
            assert!(!error.to_string().contains("tail"));
        }
    }
    #[test]
    fn named_pasteboard_without_text_is_absent_instead_of_invalid() {
        let name = name("no-format");
        run(
            "ObjC.import('AppKit'); $.NSPasteboard.generalPasteboard.clearContents;",
            &name,
            &[],
        )
        .unwrap();
        assert_eq!(read_named(&name).unwrap(), None);
    }
    #[test]
    fn named_text_preserves_unicode_whitespace_and_normalizes_all_newlines() {
        let name = name("roundtrip");
        run(
            WRITE_TEXT_SCRIPT,
            &name,
            " \t中文 café 😀\r\nline\rnext\n \t".as_bytes(),
        )
        .unwrap();
        assert_eq!(
            run(READ_TEXT_SCRIPT, &name, &[]).unwrap(),
            " \t中文 café 😀\nline\nnext\n \t".as_bytes()
        );
    }
    #[test]
    fn named_text_rejects_invalid_payload_before_clearing_existing_copy() {
        let name = name("invalid");
        run(WRITE_TEXT_SCRIPT, &name, b"keep me").unwrap();
        for invalid in [
            vec![],
            b"before\0after".to_vec(),
            vec![0xff],
            vec![b'x'; 1_048_577],
        ] {
            assert!(
                run(WRITE_TEXT_SCRIPT, &name, &invalid).is_err(),
                "invalid input was published"
            );
            assert_eq!(run(READ_TEXT_SCRIPT, &name, &[]).unwrap(), b"keep me");
        }
    }
    #[test]
    fn named_text_stale_guard_preserves_local_copy_and_receipt_tracks_origin() {
        let name = name("receipt");
        run(WRITE_TEXT_SCRIPT, &name, b"local copy").unwrap();
        assert!(
            run(
                &guarded_write_script(WRITE_TEXT_SCRIPT, u64::MAX),
                &name,
                b"remote"
            )
            .is_err()
        );
        assert_eq!(run(READ_TEXT_SCRIPT, &name, &[]).unwrap(), b"local copy");
        let emit = "\n$.NSFileHandle.fileHandleWithStandardOutput.writeData($(String(crossdeskPublicationRevision)).dataUsingEncoding($.NSUTF8StringEncoding));";
        let script = format!("{WRITE_TEXT_SCRIPT}{emit}");
        let receipt = String::from_utf8(run(&script, &name, b"remote").unwrap())
            .unwrap()
            .parse::<u64>()
            .unwrap();
        assert!(receipt > 0);
        let script = WRITE_TEXT_SCRIPT.replace("// CrossDesk receipt boundary.",
            "pasteboard.clearContents; pasteboard.setStringForType(text,'public.utf8-plain-text');\n// CrossDesk receipt boundary.");
        assert_eq!(
            run(&format!("{script}{emit}"), &name, b"remote").unwrap(),
            b"0"
        );
        assert_eq!(run(READ_TEXT_SCRIPT, &name, &[]).unwrap(), b"remote");
    }
}
