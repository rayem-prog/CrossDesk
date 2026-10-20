// CrossDesk file clipboard protocol: 2026-10-20 (GPL-3.0-only).
use super::{MAX_BATCH_BYTES, MAX_FILE_BYTES, MAX_FILES, invalid, safe_name};
use crate::protocol::{Frame, FrameKind};
use std::io;
pub type Id = [u8; 16];
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub len: u64,
}
#[derive(Clone, Debug)]
pub struct Offer {
    pub id: Id,
    pub entries: Vec<Entry>,
}
impl Offer {
    pub fn frame(&self) -> Frame {
        let mut b = self.id.to_vec();
        b.extend_from_slice(&(self.entries.len() as u16).to_be_bytes());
        for e in &self.entries {
            b.extend_from_slice(&(e.name.len() as u16).to_be_bytes());
            b.extend_from_slice(e.name.as_bytes());
            b.extend_from_slice(&e.len.to_be_bytes());
        }
        Frame::new(FrameKind::FileOffer, b)
    }
    pub fn parse(data: &[u8]) -> io::Result<Self> {
        let mut r = Reader::new(data);
        let id = r.id()?;
        let count = r.u16()? as usize;
        if count == 0 || count > MAX_FILES {
            return Err(invalid("file count exceeds 1..64"));
        }
        let mut entries = Vec::new();
        let mut total = 0u64;
        for _ in 0..count {
            let n = r.u16()? as usize;
            if n > 255 {
                return Err(invalid("filename exceeds 255 bytes"));
            }
            let name = std::str::from_utf8(r.take(n)?)
                .map_err(|_| invalid("filename is not UTF-8"))?
                .to_string();
            let len = r.u64()?;
            if !safe_name(&name) || len > MAX_FILE_BYTES {
                return Err(invalid("unsafe name or file exceeds 512 MiB"));
            }
            total = total
                .checked_add(len)
                .ok_or_else(|| invalid("batch size overflow"))?;
            if total > MAX_BATCH_BYTES {
                return Err(invalid("file batch exceeds 2 GiB"));
            }
            entries.push(Entry { name, len });
        }
        r.end()?;
        Ok(Self { id, entries })
    }
    pub fn total(&self) -> u64 {
        self.entries.iter().map(|e| e.len).sum()
    }
}
#[derive(Clone, Debug)]
pub struct Ack {
    pub id: Id,
    pub serial: u64,
    pub status: u8,
}
pub fn ack(id: Id, serial: u64, status: u8) -> Frame {
    let mut b = id.to_vec();
    b.extend_from_slice(&serial.to_be_bytes());
    b.push(status);
    Frame::new(FrameKind::FileAck, b)
}
pub fn parse_ack(b: &[u8]) -> io::Result<Ack> {
    let mut r = Reader::new(b);
    let id = r.id()?;
    let serial = r.u64()?;
    let status = r.take(1)?[0];
    r.end()?;
    if status > 2 {
        return Err(invalid("unknown file acknowledgment"));
    }
    Ok(Ack { id, serial, status })
}
pub fn request(
    kind: FrameKind,
    id: Id,
    serial: u64,
    index: u16,
    offset: u64,
    data: &[u8],
) -> Frame {
    let mut b = id.to_vec();
    b.extend_from_slice(&serial.to_be_bytes());
    if matches!(kind, FrameKind::FileData | FrameKind::FileSeal) {
        b.extend_from_slice(&index.to_be_bytes());
    }
    if kind == FrameKind::FileData {
        b.extend_from_slice(&offset.to_be_bytes());
    }
    b.extend_from_slice(data);
    Frame::new(kind, b)
}
pub fn identity(frame: &Frame) -> io::Result<(Id, u64)> {
    let mut r = Reader::new(&frame.payload);
    let id = r.id()?;
    let serial = if matches!(frame.kind, FrameKind::FileOffer | FrameKind::FileCancel) {
        0
    } else {
        r.u64()?
    };
    Ok((id, serial))
}
/// 0 published, 1 skipped (newer copy), 2 rejected/failed; no file contents or paths.
pub fn result(id: Id, status: u8) -> Frame {
    let mut b = id.to_vec();
    b.push(status);
    Frame::new(FrameKind::TransferResult, b)
}
pub fn parse_result(data: &[u8]) -> io::Result<(Id, u8)> {
    if data.len() != 17 || data[16] > 2 {
        return Err(invalid("invalid clipboard transfer result"));
    }
    Ok((data[..16].try_into().unwrap(), data[16]))
}
pub fn handles(kind: FrameKind) -> bool {
    matches!(
        kind,
        FrameKind::FileOffer
            | FrameKind::FileData
            | FrameKind::FileSeal
            | FrameKind::FileCommit
            | FrameKind::FileAck
            | FrameKind::FileCancel
            | FrameKind::TransferResult
    )
}
pub struct Reader<'a> {
    data: &'a [u8],
    offset: usize,
}
impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }
    pub fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(n)
            .ok_or_else(|| invalid("metadata overflow"))?;
        let b = self
            .data
            .get(self.offset..end)
            .ok_or_else(|| invalid("truncated file metadata"))?;
        self.offset = end;
        Ok(b)
    }
    pub fn id(&mut self) -> io::Result<Id> {
        Ok(self.take(16)?.try_into().unwrap())
    }
    pub fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub fn rest(&self) -> &'a [u8] {
        &self.data[self.offset..]
    }
    pub fn end(&self) -> io::Result<()> {
        if self.rest().is_empty() {
            Ok(())
        } else {
            Err(invalid("trailing file metadata"))
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manifests_reject_oversized_files_batches_unsafe_names_and_trailing_bytes() {
        for offer in [
            Offer {
                id: [1; 16],
                entries: vec![Entry {
                    name: "x.pdf".into(),
                    len: MAX_FILE_BYTES + 1,
                }],
            },
            Offer {
                id: [1; 16],
                entries: vec![Entry {
                    name: "../x.pdf".into(),
                    len: 1,
                }],
            },
            Offer {
                id: [1; 16],
                entries: vec![
                    Entry {
                        name: "x".into(),
                        len: MAX_FILE_BYTES
                    };
                    5
                ],
            },
            Offer {
                id: [1; 16],
                entries: vec![
                    Entry {
                        name: "x".into(),
                        len: 0
                    };
                    MAX_FILES + 1
                ],
            },
        ] {
            assert!(Offer::parse(&offer.frame().payload).is_err());
        }
        let mut f = Offer {
            id: [1; 16],
            entries: vec![Entry {
                name: "文档.pdf".into(),
                len: 0,
            }],
        }
        .frame();
        assert_eq!(
            Offer::parse(&f.payload).unwrap().entries[0].name,
            "文档.pdf"
        );
        f.payload.push(0);
        assert!(Offer::parse(&f.payload).is_err());
        assert!(parse_ack(&[0; 24]).is_err());
        assert!(parse_result(&[0; 18]).is_err());
    }
}
