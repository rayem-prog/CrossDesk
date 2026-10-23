//! Bounded, ordered input work; only adjacent monotonic motion is coalesced.
use crate::protocol::InputEvent;
use std::io;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct TimedInput {
    pub event: InputEvent,
    pub enqueued: Instant,
    pub samples: u64,
}
use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};

pub struct EventQueue {
    capacity: usize,
    state: Mutex<State>,
    ready: Condvar,
}
#[derive(Default)]
struct State {
    events: VecDeque<TimedInput>,
    closed: bool,
    peak: usize,
    peak_epoch: u64,
}
impl State {
    fn observe_peak_epoch(&mut self, epoch: u64) {
        if self.peak_epoch != epoch {
            self.peak_epoch = epoch;
            self.peak = self.events.len();
        }
    }
}
impl EventQueue {
    pub fn take_peak_depth(&self) -> usize {
        self.take_peak_in_epoch(crate::diagnostics::active_run_id())
    }
    fn take_peak_in_epoch(&self, epoch: u64) -> usize {
        let mut state = self.state.lock().unwrap();
        // Usually observed before the pop. Also cover a run starting between pop and sample.
        if state.peak_epoch != epoch {
            state.peak_epoch = epoch;
            state.peak = (state.events.len() + 1).min(self.capacity);
        }
        let peak = state.peak;
        state.peak = state.events.len();
        peak
    }
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            state: Mutex::new(State::default()),
            ready: Condvar::new(),
        }
    }
    pub fn push(&self, event: InputEvent) -> io::Result<()> {
        let mut state = self.state.lock().unwrap();
        state.observe_peak_epoch(crate::diagnostics::active_run_id());
        if state.closed {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "input queue closed",
            ));
        }
        if let Some(last) = state.events.back_mut() {
            if merge_motion(&mut last.event, &event) {
                last.samples = last.samples.saturating_add(1);
                return Ok(());
            }
        }
        if state.events.len() >= self.capacity {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "input queue overloaded; closing session to release input",
            ));
        }
        state.events.push_back(TimedInput {
            event,
            enqueued: Instant::now(),
            samples: 1,
        });
        state.peak = state.peak.max(state.events.len());
        self.ready.notify_one();
        Ok(())
    }
    pub fn try_pop(&self) -> Option<TimedInput> {
        let mut state = self.state.lock().unwrap();
        state.observe_peak_epoch(crate::diagnostics::active_run_id());
        state.events.pop_front()
    }
    pub fn recv(&self) -> Option<TimedInput> {
        let state = self.state.lock().unwrap();
        let mut state = self
            .ready
            .wait_while(state, |state| state.events.is_empty() && !state.closed)
            .unwrap();
        state.observe_peak_epoch(crate::diagnostics::active_run_id());
        state.events.pop_front()
    }
    pub fn recv_timeout(&self, timeout: Duration) -> Option<TimedInput> {
        let state = self.state.lock().unwrap();
        let (mut state, _) = self
            .ready
            .wait_timeout_while(state, timeout, |state| {
                state.events.is_empty() && !state.closed
            })
            .unwrap();
        state.observe_peak_epoch(crate::diagnostics::active_run_id());
        state.events.pop_front()
    }
    pub fn close(&self) {
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        state.events.clear();
        self.ready.notify_all();
    }
    pub fn is_closed(&self) -> bool {
        self.state.lock().unwrap().closed
    }
    pub fn len(&self) -> usize {
        self.state.lock().unwrap().events.len()
    }
}
pub fn merge_motion(left: &mut InputEvent, right: &InputEvent) -> bool {
    let (InputEvent::MouseDelta { dx: ax, dy: ay }, InputEvent::MouseDelta { dx: bx, dy: by }) =
        (&mut *left, right)
    else {
        return false;
    };
    let compatible = |a: i32, b: i32| a == 0 || b == 0 || a.signum() == b.signum();
    if !compatible(*ax, *bx) || !compatible(*ay, *by) {
        return false;
    }
    let (Some(x), Some(y)) = (ax.checked_add(*bx), ay.checked_add(*by)) else {
        return false;
    };
    *ax = x;
    *ay = y;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{KeyState, MouseButton};
    use std::sync::{Arc, mpsc};
    use std::thread;
    fn motion(dx: i32, dy: i32) -> InputEvent {
        InputEvent::MouseDelta { dx, dy }
    }

    #[test]
    fn an_eight_khz_motion_burst_is_one_work_item_without_losing_displacement() {
        let queue = EventQueue::new(256);
        let start = Instant::now();
        queue.push(motion(1, -2)).unwrap();
        let first = Instant::now();
        for _ in 1..8000 {
            queue.push(motion(1, -2)).unwrap();
        }
        assert_eq!(queue.len(), 1);
        let item = queue.try_pop().unwrap();
        assert_eq!(item.event, motion(8000, -16000));
        assert_eq!(item.samples, 8000);
        assert!(item.enqueued >= start && item.enqueued <= first);
    }
    #[test]
    fn control_events_and_reversed_axes_are_exact_boundaries() {
        let queue = EventQueue::new(256);
        let expected = vec![
            InputEvent::MouseEnter { x: 20, y: 30 },
            motion(4, 6),
            InputEvent::MouseButton {
                button: MouseButton::Left,
                down: true,
            },
            motion(5, 2),
            motion(-3, 2),
            InputEvent::MouseWheel {
                horizontal: 0,
                vertical: 1,
            },
            motion(7, 0),
            InputEvent::Key {
                scancode: 42,
                state: KeyState::Down,
            },
            motion(0, 4),
            InputEvent::MouseLeave,
        ];
        for event in &expected {
            queue.push(event.clone()).unwrap();
        }
        let actual: Vec<_> = std::iter::from_fn(|| queue.try_pop())
            .map(|v| v.event)
            .collect();
        assert_eq!(actual, expected);
    }
    #[test]
    fn merging_must_preserve_reversal_clipping_at_screen_edges() {
        let queue = EventQueue::new(8);
        for (x, y) in [(50, 50), (4, 0), (-2, 0), (0, -100), (0, 3), (7, 1)] {
            queue.push(motion(x, y)).unwrap();
        }
        let mut position = (8i64, 8i64);
        while let Some(v) = queue.try_pop() {
            let InputEvent::MouseDelta { dx, dy } = v.event else {
                unreachable!()
            };
            position.0 = (position.0 + dx as i64).clamp(0, 9);
            position.1 = (position.1 + dy as i64).clamp(0, 9);
        }
        assert_eq!(position, (9, 4));
    }
    #[test]
    fn overflow_is_explicit_and_never_mutates_the_last_queued_event() {
        let queue = EventQueue::new(2);
        queue.push(motion(i32::MAX, 1)).unwrap();
        queue.push(motion(1, 0)).unwrap();
        let err = queue.push(InputEvent::MouseLeave).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(queue.try_pop().unwrap().event, motion(i32::MAX, 1));
        assert_eq!(queue.try_pop().unwrap().event, motion(1, 0));
        assert!(queue.try_pop().is_none());
    }
    #[test]
    fn close_discards_work_and_wakes_a_waiting_consumer() {
        let queue = Arc::new(EventQueue::new(8));
        queue.push(motion(1, 1)).unwrap();
        queue.close();
        assert!(queue.is_closed());
        assert!(queue.recv().is_none());
        assert_eq!(
            queue.push(motion(1, 1)).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        let other = Arc::new(EventQueue::new(8));
        let target = other.clone();
        let (tx, rx) = mpsc::channel();
        let worker = thread::spawn(move || tx.send(target.recv().is_none()).unwrap());
        other.close();
        assert!(rx.recv_timeout(Duration::from_secs(1)).unwrap());
        worker.join().unwrap();
    }
    #[test]
    fn timed_wait_wakes_for_new_input_and_never_waits_after_close() {
        let queue = Arc::new(EventQueue::new(8));
        assert!(queue.recv_timeout(Duration::from_millis(2)).is_none());
        let target = queue.clone();
        let worker = thread::spawn(move || target.recv_timeout(Duration::from_secs(1)));
        queue.push(motion(2, 3)).unwrap();
        assert_eq!(worker.join().unwrap().unwrap().event, motion(2, 3));
        queue.close();
        assert!(queue.recv_timeout(Duration::from_secs(1)).is_none());
    }
    #[test]
    fn high_water_is_measured_at_enqueue_even_if_the_consumer_has_drained() {
        let queue = EventQueue::new(8);
        for dx in [1, -1, 1] {
            queue.push(motion(dx, 0)).unwrap();
        }
        queue.try_pop().unwrap();
        queue.try_pop().unwrap();
        assert_eq!(queue.take_peak_depth(), 3);
        assert_eq!(queue.take_peak_depth(), 1);
    }
    #[test]
    fn diagnostic_run_does_not_inherit_a_drained_historical_peak() {
        let queue = EventQueue::new(8);
        for dx in [1, -1, 1] {
            queue.push(motion(dx, 0)).unwrap();
        }
        while queue.try_pop().is_some() {}
        queue.push(motion(1, 0)).unwrap();
        queue.try_pop().unwrap();
        assert_eq!(queue.take_peak_in_epoch(7), 1);
    }
}
