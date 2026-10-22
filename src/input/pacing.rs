use super::queue::{TimedInput, merge_motion};
use crate::protocol::InputEvent;
use std::time::{Duration, Instant};

pub(crate) struct Pacer {
    interval: Duration,
    last_motion: Option<Instant>,
    pending: Option<TimedInput>,
}

impl Pacer {
    pub(crate) fn new(interval: Duration) -> Self {
        Self {
            interval,
            last_motion: None,
            pending: None,
        }
    }
    pub(crate) fn push(&mut self, input: TimedInput, now: Instant) -> [Option<TimedInput>; 2] {
        if !matches!(input.event, InputEvent::MouseDelta { .. }) {
            let pending = self.pending.take();
            if pending.is_some() {
                self.last_motion = Some(now);
            }
            return [pending, Some(input)];
        }
        let mut flushed = None;
        if let Some(pending) = self.pending.as_mut() {
            if merge_motion(&mut pending.event, &input.event) {
                pending.samples = pending.samples.saturating_add(input.samples);
                return [self.due(now), None];
            }
            // Reversal/overflow is an ordering boundary, never a lossy sum.
            flushed = self.pending.take();
            self.last_motion = Some(now);
        }
        if self
            .last_motion
            .is_none_or(|last| now >= last + self.interval)
        {
            self.last_motion = Some(now);
            [Some(input), None]
        } else {
            self.pending = Some(input);
            [flushed, None]
        }
    }
    pub(crate) fn due(&mut self, now: Instant) -> Option<TimedInput> {
        if self.deadline().is_some_and(|deadline| now >= deadline) {
            self.last_motion = Some(now);
            self.pending.take()
        } else {
            None
        }
    }
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.pending.as_ref()?;
        self.last_motion.map(|last| last + self.interval)
    }
    pub(crate) fn clear(&mut self) {
        self.pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn motion(dx: i32, now: Instant) -> TimedInput {
        TimedInput {
            event: InputEvent::MouseDelta { dx, dy: 0 },
            enqueued: now,
            samples: 1,
        }
    }
    fn delta(input: Option<TimedInput>) -> Option<i32> {
        input.map(|input| match input.event {
            InputEvent::MouseDelta { dx, .. } => dx,
            _ => panic!("expected motion"),
        })
    }

    // Delaying every first movement would reintroduce startup latency.
    #[test]
    fn first_and_idle_motion_are_immediate() {
        let now = Instant::now();
        let mut pacer = Pacer::new(Duration::from_millis(2));
        assert_eq!(delta(pacer.push(motion(4, now), now)[0].take()), Some(4));
        let later = now + Duration::from_millis(5);
        assert_eq!(
            delta(pacer.push(motion(7, later), later)[0].take()),
            Some(7)
        );
    }

    // Resetting the deadline on new samples would indefinitely postpone a stream.
    #[test]
    fn continuous_motion_uses_last_send_deadline_and_retains_oldest_sample() {
        let now = Instant::now();
        let mut pacer = Pacer::new(Duration::from_millis(2));
        pacer.push(motion(1, now), now);
        let queued = now + Duration::from_micros(300);
        assert!(pacer.push(motion(3, queued), queued)[0].is_none());
        pacer.push(motion(5, now), now + Duration::from_millis(1));
        assert_eq!(pacer.deadline(), Some(now + Duration::from_millis(2)));
        assert!(pacer.due(now + Duration::from_micros(1999)).is_none());
        let sent = pacer.due(now + Duration::from_millis(2)).unwrap();
        assert_eq!(sent.event, InputEvent::MouseDelta { dx: 8, dy: 0 });
        assert_eq!(sent.enqueued, queued);
        assert_eq!(sent.samples, 2);
    }

    // Merging reversals or wheel events changes edge-clamped pointer behavior/order.
    #[test]
    fn reversal_and_wheel_flush_pending_motion_in_order() {
        let now = Instant::now();
        let mut pacer = Pacer::new(Duration::from_millis(2));
        pacer.push(motion(1, now), now);
        pacer.push(motion(3, now), now);
        let mut out = pacer.push(motion(-2, now), now);
        assert_eq!(delta(out[0].take()), Some(3));
        assert!(out[1].is_none());
        let wheel = InputEvent::MouseWheel {
            horizontal: 0,
            vertical: 1,
        };
        let mut out = pacer.push(
            TimedInput {
                event: wheel.clone(),
                enqueued: now,
                samples: 1,
            },
            now,
        );
        assert_eq!(delta(out[0].take()), Some(-2));
        assert_eq!(out[1].take().unwrap().event, wheel);
        assert!(pacer.deadline().is_none());
    }

    // Replaying pending input after disconnect would move the next session's pointer.
    #[test]
    fn clear_discards_pending_motion() {
        let now = Instant::now();
        let mut pacer = Pacer::new(Duration::from_millis(2));
        pacer.push(motion(1, now), now);
        pacer.push(motion(9, now), now);
        assert!(pacer.deadline().is_some());
        pacer.clear();
        assert!(pacer.due(now + Duration::from_secs(1)).is_none());
    }
}
