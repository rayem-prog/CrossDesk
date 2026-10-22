// CrossDesk file clipboard protocol: 2026-10-20 (GPL-3.0-only).
use super::{MAX_BATCH_BYTES, MAX_FILE_BYTES, MAX_FILES, invalid, safe_name};
use crate::protocol::{Frame, FrameKind};
use std::io;
pub type Id = [u8; 16];
pub const MAX_NODES: usize = 10_000;
pub const MAX_DEPTH: usize = 32;
pub const MAX_MANIFEST: usize = 4 * 1024 * 1024;
#[derive(Clone, Debug)]
pub struct Entry {
    pub parent: Option<u16>,
    pub directory: bool,
    pub name: String,
    pub len: u64,
}
#[derive(Clone, Debug)]
pub struct Offer {
    pub id: Id,
    pub entries: Vec<Entry>,
}
impl Offer {
    fn metadata(&self) -> Vec<u8> {
        let mut b = (self.entries.len() as u16).to_be_bytes().to_vec();
        for e in &self.entries {
            b.extend_from_slice(&e.parent.unwrap_or(u16::MAX).to_be_bytes());
            b.push(u8::from(e.directory));
            b.extend_from_slice(&(e.name.len() as u16).to_be_bytes());
            b.extend_from_slice(e.name.as_bytes());
            b.extend_from_slice(&e.len.to_be_bytes());
        }
        b
    }
    pub fn frames(&self) -> Vec<Frame> {
        let data = self.metadata();
        let first = data.len().min(crate::protocol::MAX_FRAME_PAYLOAD_SIZE - 20);
        let mut b = self.id.to_vec();
        b.extend_from_slice(&(data.len() as u32).to_be_bytes());
        b.extend_from_slice(&data[..first]);
        let mut out = vec![Frame::new(FrameKind::FileOffer, b)];
        for (n, part) in data[first..]
            .chunks(crate::protocol::MAX_FRAME_PAYLOAD_SIZE - 24)
            .enumerate()
        {
            out.push(request(
                FrameKind::FileManifest,
                self.id,
                n as u64 + 1,
                0,
                0,
                part,
            ));
        }
        out
    }
    #[cfg(test)]
    pub fn frame(&self) -> Frame {
        self.frames().remove(0)
    }
    #[cfg(test)]
    pub fn parse(data: &[u8]) -> io::Result<Self> {
        let mut r = Reader::new(data);
        let id = r.id()?;
        let len = r.u32()? as usize;
        if len != r.rest().len() {
            return Err(invalid("incomplete or trailing manifest"));
        }
        Self::parse_metadata(id, r.rest())
    }
    pub fn parse_metadata(id: Id, data: &[u8]) -> io::Result<Self> {
        if data.len() > MAX_MANIFEST {
            return Err(invalid("manifest exceeds 4 MiB"));
        }
        let mut r = Reader::new(data);
        let count = r.u16()? as usize;
        if count == 0 || count > MAX_NODES {
            return Err(invalid("tree must contain 1..10,000 entries"));
        }
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let parent = match r.u16()? {
                u16::MAX => None,
                v => Some(v),
            };
            let directory = match r.take(1)?[0] {
                0 => false,
                1 => true,
                _ => return Err(invalid("unknown entry type")),
            };
            let n = r.u16()? as usize;
            if n > 255 {
                return Err(invalid("filename exceeds 255 bytes"));
            }
            let name = std::str::from_utf8(r.take(n)?)
                .map_err(|_| invalid("filename is not UTF-8"))?
                .to_owned();
            let len = r.u64()?;
            entries.push(Entry {
                parent,
                directory,
                name,
                len,
            });
        }
        r.end()?;
        let offer = Self { id, entries };
        offer.validate()?;
        Ok(offer)
    }
    pub fn validate(&self) -> io::Result<()> {
        if self.entries.is_empty() || self.entries.len() > MAX_NODES {
            return Err(invalid("tree must contain 1..10,000 entries"));
        }
        let mut depths = Vec::with_capacity(self.entries.len());
        let mut roots = 0;
        let mut total = 0u64;
        for (index, e) in self.entries.iter().enumerate() {
            if !safe_name(&e.name) || e.len > MAX_FILE_BYTES || (e.directory && e.len != 0) {
                return Err(invalid(
                    "unsafe name, invalid directory length or file exceeds 512 MiB",
                ));
            }
            let depth = if let Some(parent) = e.parent {
                let parent = parent as usize;
                if parent >= index || !self.entries[parent].directory {
                    return Err(invalid(
                        "parent must be an earlier directory; cycles and forward references are forbidden",
                    ));
                }
                depths[parent] + 1
            } else {
                roots += 1;
                1
            };
            if roots > MAX_FILES || depth > MAX_DEPTH {
                return Err(invalid("tree exceeds 64 top-level items or 32 levels"));
            }
            depths.push(depth);
            total = total
                .checked_add(e.len)
                .ok_or_else(|| invalid("batch size overflow"))?;
            if total > MAX_BATCH_BYTES {
                return Err(invalid("file batch exceeds 2 GiB"));
            }
        }
        if self.metadata().len() > MAX_MANIFEST {
            return Err(invalid("manifest exceeds 4 MiB"));
        }
        Ok(())
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
            | FrameKind::FileManifest
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
    pub fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
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
    fn tree_validation_rejects_cycles_files_as_parents_traversal_and_resource_exhaustion() {
        let directory = Entry {
            parent: None,
            directory: true,
            name: "root".into(),
            len: 0,
        };
        let file = Entry {
            parent: None,
            directory: false,
            name: "file".into(),
            len: 0,
        };
        let bad = vec![
            vec![Entry {
                parent: Some(0),
                ..directory.clone()
            }],
            vec![
                Entry {
                    parent: Some(1),
                    ..directory.clone()
                },
                directory.clone(),
            ],
            vec![
                file,
                Entry {
                    parent: Some(0),
                    ..directory.clone()
                },
            ],
            vec![Entry {
                name: "../outside".into(),
                ..directory.clone()
            }],
            vec![Entry {
                len: 1,
                ..directory.clone()
            }],
            vec![directory.clone(); 10_001],
            (0..33)
                .map(|i| Entry {
                    parent: if i == 0 { None } else { Some(i - 1) },
                    ..directory.clone()
                })
                .collect(),
        ];
        for entries in bad {
            let offer = Offer {
                id: [3; 16],
                entries,
            };
            assert!(Offer::parse_metadata(offer.id, &offer.metadata()).is_err());
        }
    }
    #[test]
    fn manifests_reject_oversized_files_batches_unsafe_names_and_trailing_bytes() {
        for offer in [
            Offer {
                id: [1; 16],
                entries: vec![Entry {
                    parent: None,
                    directory: false,
                    name: "x.pdf".into(),
                    len: MAX_FILE_BYTES + 1,
                }],
            },
            Offer {
                id: [1; 16],
                entries: vec![Entry {
                    parent: None,
                    directory: false,
                    name: "../x.pdf".into(),
                    len: 1,
                }],
            },
            Offer {
                id: [1; 16],
                entries: vec![
                    Entry {
                        parent: None,
                        directory: false,
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
                        parent: None,
                        directory: false,
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
                parent: None,
                directory: false,
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
