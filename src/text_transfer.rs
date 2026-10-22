//! Bounded, memory-only plain text clipboard transfer.
use crate::protocol::{CHUNK_SIZE, Frame, FrameKind};
use sha2::{Digest, Sha256};
use std::io;
use std::time::{Duration, Instant};

pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
struct RejectedText(&'static str);
impl std::fmt::Display for RejectedText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for RejectedText {}

pub fn is_rejection(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|error| error.is::<RejectedText>())
}

pub fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, RejectedText(message))
}

/// Normalize line endings without trimming or changing any other character.
pub fn normalize(text: &str) -> io::Result<String> {
    let mut normalized = String::with_capacity(text.len().min(MAX_TEXT_BYTES));
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\0' {
            return Err(invalid("text contains a NUL character"));
        }
        if character == '\r' {
            if characters.peek() == Some(&'\n') {
                characters.next();
            }
            normalized.push('\n');
        } else {
            normalized.push(character);
        }
        if normalized.len() > MAX_TEXT_BYTES {
            return Err(invalid("text exceeds the 1 MiB limit"));
        }
    }
    if normalized.is_empty() {
        return Err(invalid("text is empty"));
    }
    Ok(normalized)
}

#[derive(Debug)]
pub struct Transfer {
    pub id: [u8; 16],
    pub text: String,
}
impl Transfer {
    pub fn from_text(text: &str) -> io::Result<Self> {
        let text = normalize(text)?;
        let mut id = [0; 16];
        getrandom::getrandom(&mut id)
            .map_err(|_| io::Error::other("text transfer identity generation failed"))?;
        Ok(Self { id, text })
    }

    pub fn visit_frames(&self, mut send: impl FnMut(Frame) -> io::Result<()>) -> io::Result<()> {
        validate_canonical(&self.text)?;
        let mut payload = self.id.to_vec();
        payload.extend_from_slice(&(self.text.len() as u32).to_be_bytes());
        payload.extend_from_slice(&Sha256::digest(self.text.as_bytes()));
        send(Frame::new(FrameKind::TextStart, payload))?;
        for (index, data) in self.text.as_bytes().chunks(CHUNK_SIZE).enumerate() {
            let mut payload = self.id.to_vec();
            payload.extend_from_slice(&((index * CHUNK_SIZE) as u32).to_be_bytes());
            payload.extend_from_slice(data);
            send(Frame::new(FrameKind::TextChunk, payload))?;
        }
        send(Frame::new(FrameKind::TextEnd, self.id.to_vec()))
    }
}

fn validate_canonical(text: &str) -> io::Result<()> {
    if text.is_empty() || text.len() > MAX_TEXT_BYTES || text.contains(['\0', '\r']) {
        return Err(invalid("invalid canonical text"));
    }
    Ok(())
}

pub fn cancel(id: [u8; 16]) -> Frame {
    Frame::new(FrameKind::TextCancel, id.to_vec())
}

pub fn handles(kind: FrameKind) -> bool {
    matches!(
        kind,
        FrameKind::TextStart | FrameKind::TextChunk | FrameKind::TextEnd | FrameKind::TextCancel
    )
}

/// Extract only from a recognized frame with enough metadata for its kind.
pub fn identity(frame: &Frame) -> Option<[u8; 16]> {
    let valid_length = match frame.kind {
        FrameKind::TextStart => frame.payload.len() >= 52,
        FrameKind::TextChunk => frame.payload.len() >= 20,
        FrameKind::TextEnd | FrameKind::TextCancel => frame.payload.len() == 16,
        _ => false,
    };
    valid_length.then(|| frame.payload[..16].try_into().unwrap())
}

#[derive(Default)]
pub struct Incoming {
    current: Option<Assembly>,
}
struct Assembly {
    id: [u8; 16],
    length: usize,
    digest: [u8; 32],
    bytes: Vec<u8>,
    progress: Instant,
}
impl Incoming {
    pub fn active_id(&self) -> Option<[u8; 16]> {
        self.current.as_ref().map(|current| current.id)
    }

    pub fn expire(&mut self) -> Option<[u8; 16]> {
        if self
            .current
            .as_ref()
            .is_some_and(|current| current.progress.elapsed() >= IDLE_TIMEOUT)
        {
            return self.current.take().map(|current| current.id);
        }
        None
    }

    pub fn reset(&mut self) {
        self.current = None;
    }

    pub fn receive(&mut self, frame: Frame) -> io::Result<Option<Transfer>> {
        self.expire();
        let result = self.receive_checked(frame);
        if result.is_err() {
            self.reset();
        }
        result
    }

    fn receive_checked(&mut self, frame: Frame) -> io::Result<Option<Transfer>> {
        let id = identity(&frame).ok_or_else(|| invalid("invalid text frame metadata"))?;
        match frame.kind {
            FrameKind::TextStart => {
                if frame.payload.len() != 52 {
                    return Err(invalid("invalid text start length"));
                }
                let length = u32::from_be_bytes(frame.payload[16..20].try_into().unwrap()) as usize;
                if length == 0 || length > MAX_TEXT_BYTES {
                    return Err(invalid("invalid text transfer length"));
                }
                self.current = Some(Assembly {
                    id,
                    length,
                    digest: frame.payload[20..52].try_into().unwrap(),
                    bytes: Vec::with_capacity(length),
                    progress: Instant::now(),
                });
                Ok(None)
            }
            FrameKind::TextChunk => {
                let current = self
                    .current
                    .as_mut()
                    .ok_or_else(|| invalid("text chunk without start"))?;
                let offset = u32::from_be_bytes(frame.payload[16..20].try_into().unwrap()) as usize;
                let data = &frame.payload[20..];
                if id != current.id
                    || offset != current.bytes.len()
                    || data.is_empty()
                    || data.len() > CHUNK_SIZE
                    || data.len() > current.length - current.bytes.len()
                {
                    return Err(invalid("invalid text chunk identity, offset or length"));
                }
                current.bytes.extend_from_slice(data);
                current.progress = Instant::now();
                Ok(None)
            }
            FrameKind::TextEnd => {
                let current = self
                    .current
                    .take()
                    .ok_or_else(|| invalid("text completion without start"))?;
                if id != current.id
                    || current.bytes.len() != current.length
                    || <[u8; 32]>::from(Sha256::digest(&current.bytes)) != current.digest
                {
                    return Err(invalid("incomplete or corrupt text transfer"));
                }
                let text =
                    String::from_utf8(current.bytes).map_err(|_| invalid("invalid UTF-8 text"))?;
                validate_canonical(&text)?;
                Ok(Some(Transfer { id, text }))
            }
            FrameKind::TextCancel => {
                if self.active_id().is_some_and(|active| active != id) {
                    return Err(invalid("text cancellation identity mismatch"));
                }
                self.reset();
                Ok(None)
            }
            _ => Err(invalid("unexpected text frame")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{CHUNK_SIZE, MAX_FRAME_PAYLOAD_SIZE, read_frame, write_frame};
    use sha2::{Digest, Sha256};

    #[test]
    fn text_rejections_are_distinct_from_invalid_image_or_file_data() {
        assert!(is_rejection(&normalize("bad\0text").unwrap_err()));
        assert!(!is_rejection(&io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid image"
        )));
    }

    fn start(id: [u8; 16], data: &[u8]) -> Frame {
        let mut payload = id.to_vec();
        payload.extend_from_slice(&(data.len() as u32).to_be_bytes());
        payload.extend_from_slice(&Sha256::digest(data));
        Frame::new(FrameKind::TextStart, payload)
    }
    fn chunk(id: [u8; 16], offset: u32, data: &[u8]) -> Frame {
        let mut payload = id.to_vec();
        payload.extend_from_slice(&offset.to_be_bytes());
        payload.extend_from_slice(data);
        Frame::new(FrameKind::TextChunk, payload)
    }
    fn finish(data: &[u8]) -> io::Result<Option<Transfer>> {
        let mut receiver = Incoming::default();
        receiver.receive(start([1; 16], data))?;
        receiver.receive(chunk([1; 16], 0, data))?;
        receiver.receive(Frame::new(FrameKind::TextEnd, vec![1; 16]))
    }
    #[test]
    fn normalization_preserves_whitespace_and_converts_every_newline() {
        assert_eq!(normalize(" \tA\r\nB\rC\n ").unwrap(), " \tA\nB\nC\n ");
        assert_eq!(normalize(" \n\t").unwrap(), " \n\t");
    }
    #[test]
    fn normalization_rejects_empty_nul_and_oversize_without_exposing_text() {
        for text in [
            "".to_string(),
            "secret\0value".into(),
            "z".repeat(MAX_TEXT_BYTES + 1),
        ] {
            let error = normalize(&text).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(!error.to_string().contains("secret"));
        }
        assert_eq!(
            normalize(&"\r\n".repeat(MAX_TEXT_BYTES)).unwrap().len(),
            MAX_TEXT_BYTES
        );
    }
    #[test]
    fn maximum_text_roundtrips_in_serializable_frames_with_split_multibyte_utf8() {
        let text = format!(
            "{}猫{}",
            "a".repeat(CHUNK_SIZE - 1),
            "b".repeat(MAX_TEXT_BYTES - CHUNK_SIZE - 2)
        );
        assert_eq!(text.len(), MAX_TEXT_BYTES);
        let transfer = Transfer::from_text(&text).unwrap();
        let mut receiver = Incoming::default();
        let mut result = None;
        transfer
            .visit_frames(|frame| {
                assert!(frame.payload.len() <= MAX_FRAME_PAYLOAD_SIZE);
                if frame.kind == FrameKind::TextChunk {
                    assert!(frame.payload.len() <= CHUNK_SIZE + 20);
                }
                let mut wire = Vec::new();
                write_frame(&mut wire, &frame)?;
                result = receiver.receive(read_frame(&mut &wire[..])?)?;
                Ok(())
            })
            .unwrap();
        let result = result.unwrap();
        assert_eq!(result.text, text);
        assert_eq!(result.id, transfer.id);
        assert_eq!(receiver.active_id(), None);
    }
    #[test]
    fn sender_normalizes_and_rejects_invalid_public_transfer_before_sending() {
        assert_eq!(Transfer::from_text("a\rb\r\nc").unwrap().text, "a\nb\nc");
        assert!(Transfer::from_text("").is_err());
        let invalid = Transfer {
            id: [1; 16],
            text: "private\rvalue".into(),
        };
        let mut sent = Vec::new();
        assert!(
            invalid
                .visit_frames(|f| {
                    sent.push(f);
                    Ok(())
                })
                .is_err()
        );
        assert!(sent.is_empty());
    }
    #[test]
    fn receiver_rejects_noncanonical_or_invalid_text_even_with_correct_hash() {
        for data in [b"x\0y".as_slice(), b"x\ry", b"x\r\ny", &[0xff]] {
            assert!(finish(data).is_err());
        }
        assert_eq!(finish(b" \n\t").unwrap().unwrap().text, " \n\t");
    }
    #[test]
    fn receiver_rejects_checksum_and_incomplete_completion() {
        let mut receiver = Incoming::default();
        receiver.receive(start([1; 16], b"hello")).unwrap();
        receiver.receive(chunk([1; 16], 0, b"jello")).unwrap();
        assert!(
            receiver
                .receive(Frame::new(FrameKind::TextEnd, vec![1; 16]))
                .is_err()
        );
        receiver.receive(start([1; 16], b"hello")).unwrap();
        assert!(
            receiver
                .receive(Frame::new(FrameKind::TextEnd, vec![1; 16]))
                .is_err()
        );
        assert!(receiver.active_id().is_none());
    }
    #[test]
    fn invalid_frames_discard_active_state() {
        let mut malformed_start = start([1; 16], b"hello");
        malformed_start.payload.push(0);
        let mut oversize_start = start([1; 16], b"hello");
        oversize_start.payload[16..20]
            .copy_from_slice(&((MAX_TEXT_BYTES + 1) as u32).to_be_bytes());
        for bad in [
            malformed_start,
            oversize_start,
            start([1; 16], b""),
            chunk([2; 16], 0, b"h"),
            chunk([1; 16], 1, b"h"),
            chunk([1; 16], 0, b""),
            chunk([1; 16], 0, b"toolong"),
            chunk([1; 16], 0, &vec![b'a'; CHUNK_SIZE + 1]),
            Frame::new(FrameKind::TextChunk, vec![1; 19]),
            Frame::new(FrameKind::TextEnd, vec![1; 17]),
            Frame::new(FrameKind::TextCancel, vec![1; 17]),
            Frame::new(FrameKind::TextCancel, vec![2; 16]),
            Frame::new(FrameKind::Ping, vec![]),
        ] {
            let mut receiver = Incoming::default();
            receiver.receive(start([1; 16], b"hello")).unwrap();
            assert!(receiver.receive(bad).is_err());
            assert_eq!(receiver.active_id(), None);
        }
    }
    #[test]
    fn cancel_reset_and_replacement_drop_assembly() {
        let mut receiver = Incoming::default();
        receiver.receive(start([1; 16], b"hello")).unwrap();
        receiver.receive(start([2; 16], b"world")).unwrap();
        assert_eq!(receiver.active_id(), Some([2; 16]));
        assert!(receiver.receive(cancel([2; 16])).unwrap().is_none());
        assert_eq!(receiver.active_id(), None);
        receiver.receive(start([1; 16], b"hello")).unwrap();
        receiver.reset();
        assert_eq!(receiver.active_id(), None);
        assert!(receiver.receive(chunk([1; 16], 0, b"hello")).is_err());
    }
    #[test]
    fn timeout_discards_idle_state_and_is_enforced_on_receive() {
        let mut receiver = Incoming::default();
        receiver.receive(start([1; 16], b"hello")).unwrap();
        assert_eq!(receiver.expire(), None);
        receiver.current.as_mut().unwrap().progress = Instant::now() - Duration::from_secs(31);
        assert_eq!(receiver.expire(), Some([1; 16]));
        assert_eq!(receiver.expire(), None);
        receiver.receive(start([1; 16], b"hello")).unwrap();
        receiver.current.as_mut().unwrap().progress = Instant::now() - Duration::from_secs(31);
        assert!(receiver.receive(chunk([1; 16], 0, b"hello")).is_err());
        assert_eq!(receiver.active_id(), None);
    }
    #[test]
    fn identity_bounds_and_kinds_do_not_panic_or_accept_unrelated_frames() {
        for kind in [
            FrameKind::TextStart,
            FrameKind::TextChunk,
            FrameKind::TextEnd,
            FrameKind::TextCancel,
        ] {
            assert!(handles(kind));
            for len in 0..16 {
                assert_eq!(identity(&Frame::new(kind, vec![1; len])), None);
            }
        }
        assert_eq!(identity(&start([3; 16], b"a")), Some([3; 16]));
        assert_eq!(identity(&chunk([4; 16], 0, b"a")), Some([4; 16]));
        assert_eq!(identity(&cancel([5; 16])), Some([5; 16]));
        assert!(!handles(FrameKind::Clipboard));
        assert_eq!(
            identity(&Frame::new(FrameKind::Clipboard, vec![1; 52])),
            None
        );
    }
}
