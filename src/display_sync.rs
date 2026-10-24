//! Bounded logical display geometry and the ordered resize barrier.
use crate::protocol::{Frame, FrameKind, InputEvent};
use std::io;

pub const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenUpdate {
    pub revision: u64,
    pub width: u32,
    pub height: u32,
}
impl ScreenUpdate {
    pub fn frame(self) -> Frame {
        let mut payload = self.revision.to_be_bytes().to_vec();
        payload.extend_from_slice(&self.width.to_be_bytes());
        payload.extend_from_slice(&self.height.to_be_bytes());
        Frame::new(FrameKind::ScreenUpdate, payload)
    }
    pub fn decode(payload: &[u8]) -> io::Result<Self> {
        if payload.len() != 16 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid screen update length",
            ));
        }
        let update = Self {
            revision: u64::from_be_bytes(payload[..8].try_into().unwrap()),
            width: u32::from_be_bytes(payload[8..12].try_into().unwrap()),
            height: u32::from_be_bytes(payload[12..].try_into().unwrap()),
        };
        if update.revision == 0 || !valid_size((update.width, update.height)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid screen update geometry or revision",
            ));
        }
        Ok(update)
    }
}

pub struct ClientAlignment {
    size: (u32, u32),
    enabled: bool,
    revision: u64,
    pending: Option<u64>,
}
impl ClientAlignment {
    pub fn new(size: (u32, u32), enabled: bool) -> Self {
        Self {
            size,
            enabled,
            revision: 0,
            pending: None,
        }
    }
    pub fn observe(&mut self, size: (u32, u32)) -> Option<ScreenUpdate> {
        if !self.enabled || size == self.size || !valid_size(size) {
            return None;
        }
        self.revision = self.revision.checked_add(1)?;
        self.size = size;
        self.pending = Some(self.revision);
        Some(ScreenUpdate {
            revision: self.revision,
            width: size.0,
            height: size.1,
        })
    }
    pub fn filter(&mut self, event: InputEvent) -> Option<InputEvent> {
        if let InputEvent::ScreenSync { revision } = event {
            if self.pending == Some(revision) {
                self.pending = None;
            }
            return None;
        }
        self.pending.is_none().then_some(event)
    }
}
fn valid_size(size: (u32, u32)) -> bool {
    (1..=32768).contains(&size.0) && (1..=32768).contains(&size.1)
}

/// Drives native observation on the existing input thread, even without input.
pub struct AlignmentDriver {
    alignment: ClientAlignment,
    enabled: bool,
    last_poll: std::time::Instant,
    pending_since: Option<std::time::Instant>,
}
impl AlignmentDriver {
    pub fn new(size: (u32, u32), enabled: bool, now: std::time::Instant) -> Self {
        Self {
            alignment: ClientAlignment::new(size, enabled),
            enabled,
            last_poll: now,
            pending_since: None,
        }
    }
    pub fn step(
        &mut self,
        event: Option<InputEvent>,
        now: std::time::Instant,
        hello_sent: bool,
        read_size: impl FnOnce() -> (u32, u32),
        mut apply: impl FnMut(InputEvent) -> io::Result<()>,
        mut send: impl FnMut(ScreenUpdate) -> io::Result<()>,
    ) -> io::Result<()> {
        if self.enabled
            && hello_sent
            && now.saturating_duration_since(self.last_poll) >= POLL_INTERVAL
        {
            self.last_poll = now;
            if let Some(update) = self.alignment.observe(read_size()) {
                self.pending_since = Some(now);
                apply(InputEvent::MouseLeave)?;
                send(update)?;
            }
        }
        if let Some(event) = event {
            if let Some(event) = self.alignment.filter(event) {
                apply(event)?;
            }
            if self.alignment.pending.is_none() {
                self.pending_since = None;
            }
        }
        if self.pending_since.is_some_and(|start| {
            now.saturating_duration_since(start) >= std::time::Duration::from_secs(5)
        }) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "display synchronization fence timed out",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{self, InputEvent, KeyState};

    #[test]
    fn resize_blocks_stale_input_until_the_exact_ordered_fence() {
        let mut sync = ClientAlignment::new((1920, 1080), true);
        let update = sync.observe((1512, 982)).unwrap();
        assert_eq!(
            (update.revision, update.width, update.height),
            (1, 1512, 982)
        );
        assert!(
            sync.filter(InputEvent::MouseDelta { dx: 200, dy: 50 })
                .is_none()
        );
        assert!(
            sync.filter(InputEvent::Key {
                scancode: 30,
                state: KeyState::Down
            })
            .is_none()
        );
        assert!(sync.filter(InputEvent::MouseLeave).is_none());
        assert!(
            sync.filter(InputEvent::ScreenSync { revision: 2 })
                .is_none()
        );
        assert!(
            sync.filter(InputEvent::MouseEnter { x: 8, y: 491 })
                .is_none()
        );
        assert!(
            sync.filter(InputEvent::ScreenSync { revision: 1 })
                .is_none()
        );
        assert_eq!(
            sync.filter(InputEvent::MouseEnter { x: 8, y: 491 }),
            Some(InputEvent::MouseEnter { x: 8, y: 491 })
        );
    }

    #[test]
    fn rapid_resizes_cannot_be_unlocked_by_a_stale_fence() {
        let mut sync = ClientAlignment::new((1920, 1080), true);
        sync.observe((1512, 982)).unwrap();
        let second = sync.observe((1280, 720)).unwrap();
        assert_eq!(second.revision, 2);
        sync.filter(InputEvent::ScreenSync { revision: 1 });
        assert!(
            sync.filter(InputEvent::MouseDelta { dx: 1, dy: 1 })
                .is_none()
        );
        sync.filter(InputEvent::ScreenSync { revision: 2 });
        assert_eq!(
            sync.filter(InputEvent::MouseDelta { dx: 1, dy: 1 }),
            Some(InputEvent::MouseDelta { dx: 1, dy: 1 })
        );
    }

    #[test]
    fn unchanged_invalid_and_legacy_samples_emit_no_update_or_gate() {
        let mut sync = ClientAlignment::new((1512, 982), true);
        for size in [
            (1512, 982),
            (0, 982),
            (1512, 0),
            (32769, 982),
            (1512, u32::MAX),
        ] {
            assert!(sync.observe(size).is_none());
        }
        assert_eq!(sync.observe((32768, 1)).unwrap().revision, 1);
        let mut legacy = ClientAlignment::new((1512, 982), false);
        assert!(legacy.observe((1280, 720)).is_none());
        assert_eq!(
            legacy.filter(InputEvent::MouseLeave),
            Some(InputEvent::MouseLeave)
        );
    }

    #[test]
    fn update_and_fence_use_exact_bounded_wire_encodings() {
        let update = ScreenUpdate {
            revision: 9,
            width: 1512,
            height: 982,
        };
        let frame = update.frame();
        assert_eq!(frame.kind, protocol::FrameKind::ScreenUpdate);
        assert_eq!(
            frame.payload,
            vec![0, 0, 0, 0, 0, 0, 0, 9, 0, 0, 5, 232, 0, 0, 3, 214]
        );
        assert_eq!(ScreenUpdate::decode(&frame.payload).unwrap(), update);
        let fence = InputEvent::ScreenSync { revision: 9 };
        assert_eq!(
            protocol::encode_input(&fence),
            vec![7, 0, 0, 0, 0, 0, 0, 0, 9]
        );
        assert_eq!(
            protocol::decode_input(&[7, 0, 0, 0, 0, 0, 0, 0, 9]).unwrap(),
            fence
        );
        for bad in [
            vec![7],
            vec![7, 0, 0, 0, 0, 0, 0, 0, 0],
            vec![7, 0, 0, 0, 0, 0, 0, 0, 1, 0],
        ] {
            assert!(protocol::decode_input(&bad).is_err());
        }
        for bad in [
            vec![],
            vec![0; 16],
            vec![0; 17],
            ScreenUpdate {
                revision: 1,
                width: 0,
                height: 982,
            }
            .frame()
            .payload,
            ScreenUpdate {
                revision: 1,
                width: 1512,
                height: 32769,
            }
            .frame()
            .payload,
        ] {
            assert!(ScreenUpdate::decode(&bad).is_err());
        }
    }

    #[test]
    fn capability_keeps_v7_layout_and_old_peers_unchanged() {
        let hello = protocol::decode_hello(&[0, 7, 1, 16]).unwrap();
        assert!(hello.screen_updates);
        let screen = protocol::decode_hello(&[0, 7, 2, 31, 0, 0, 5, 232, 0, 0, 3, 214]).unwrap();
        assert!(screen.screen_updates);
        assert_eq!(
            (screen.screen_width, screen.screen_height),
            (Some(1512), Some(982))
        );
        for flags in [0, 3, 7, 15] {
            assert!(
                !protocol::decode_hello(&[0, 7, 1, flags])
                    .unwrap()
                    .screen_updates
            );
        }
        let payload = protocol::hello_payload_with_screen(1512, 982);
        assert_eq!(payload.len(), 12);
        #[cfg(any(windows, target_os = "macos"))]
        assert!(protocol::decode_hello(&payload).unwrap().screen_updates);
    }

    #[test]
    fn idle_polling_is_bounded_and_never_sends_before_hello() {
        use std::time::{Duration, Instant};
        let start = Instant::now();
        let mut driver = AlignmentDriver::new((1920, 1080), true, start);
        let mut applied = Vec::new();
        let mut updates = Vec::new();
        driver
            .step(
                None,
                start + Duration::from_secs(1),
                false,
                || panic!("queried before Hello"),
                |e| {
                    applied.push(e);
                    Ok(())
                },
                |u| {
                    updates.push(u);
                    Ok(())
                },
            )
            .unwrap();
        assert!(updates.is_empty());
        driver
            .step(
                None,
                start + Duration::from_secs(1),
                true,
                || (1512, 982),
                |e| {
                    applied.push(e);
                    Ok(())
                },
                |u| {
                    updates.push(u);
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(applied, vec![InputEvent::MouseLeave]);
        assert_eq!(
            updates,
            vec![ScreenUpdate {
                revision: 1,
                width: 1512,
                height: 982
            }]
        );
        driver
            .step(
                Some(InputEvent::Key {
                    scancode: 30,
                    state: KeyState::Down,
                }),
                start + Duration::from_millis(1100),
                true,
                || panic!("polled more than four times per second"),
                |e| {
                    applied.push(e);
                    Ok(())
                },
                |_| panic!("duplicate update"),
            )
            .unwrap();
        assert_eq!(applied, vec![InputEvent::MouseLeave]);
        driver
            .step(
                Some(InputEvent::ScreenSync { revision: 1 }),
                start + Duration::from_millis(1101),
                true,
                || panic!("early poll"),
                |_| panic!("fence reached native sink"),
                |_| panic!("duplicate update"),
            )
            .unwrap();
        driver
            .step(
                Some(InputEvent::MouseEnter { x: 8, y: 491 }),
                start + Duration::from_millis(1102),
                true,
                || panic!("early poll"),
                |e| {
                    applied.push(e);
                    Ok(())
                },
                |_| panic!("duplicate update"),
            )
            .unwrap();
        assert_eq!(
            applied.last(),
            Some(&InputEvent::MouseEnter { x: 8, y: 491 })
        );
    }

    #[test]
    fn a_missing_geometry_fence_times_out_instead_of_silently_freezing_input() {
        use std::time::{Duration, Instant};
        let start = Instant::now();
        let mut driver = AlignmentDriver::new((1920, 1080), true, start);
        driver
            .step(
                None,
                start + POLL_INTERVAL,
                true,
                || (1512, 982),
                |_| Ok(()),
                |_| Ok(()),
            )
            .unwrap();
        let error = driver
            .step(
                None,
                start + Duration::from_secs(6),
                true,
                || (1512, 982),
                |_| Ok(()),
                |_| Ok(()),
            )
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    }

    #[test]
    fn invalid_native_poll_keeps_the_previous_display_and_does_not_release_input() {
        let now = std::time::Instant::now();
        let mut driver = AlignmentDriver::new((1512, 982), true, now);
        driver
            .step(
                Some(InputEvent::MouseDelta { dx: 1, dy: 2 }),
                now + POLL_INTERVAL,
                true,
                || (0, 982),
                |event| {
                    assert_eq!(event, InputEvent::MouseDelta { dx: 1, dy: 2 });
                    Ok(())
                },
                |_| panic!("invalid native bounds sent to peer"),
            )
            .unwrap();
        driver
            .step(
                None,
                now + POLL_INTERVAL * 2,
                true,
                || (1512, 982),
                |_| panic!("unchanged geometry released input"),
                |_| panic!("previous geometry was discarded"),
            )
            .unwrap();
    }
}
