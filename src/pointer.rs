use crate::server::Edge;

const EDGE_TRIGGER_MARGIN: i32 = 6;
const EDGE_REARM_DISTANCE: i32 = 8;
const RETURN_EDGE_MARGIN: i32 = 3;
const RETURN_PUSH_THRESHOLD: i32 = 10;
const LOCAL_RETURN_INSET: i32 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionAction {
    Local,
    EnterRemote { x: i32, y: i32 },
    MoveRemote { dx: i32, dy: i32 },
    ReturnLocal { x: i32, y: i32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PointerPhase {
    // Local is armed only after the pointer has moved away from the edge once;
    // this avoids immediately bouncing back to the remote screen after return.
    Local {
        armed: bool,
    },
    Remote {
        // Distance pushed toward the local screen while already touching the return edge.
        accumulated_return_push: i32,
        return_armed: bool,
    },
}

pub struct PointerRouter {
    edge: Edge,
    local_size: (i32, i32),
    remote_size: (i32, i32),
    local_pos: (i32, i32),
    remote_pos: (i32, i32),
    phase: PointerPhase,
    local_buttons_down: usize,
}

impl PointerRouter {
    pub fn new(edge: Edge, local_size: (i32, i32), remote_size: (i32, i32)) -> Self {
        Self {
            edge,
            local_size: valid_size(local_size),
            remote_size: valid_size(remote_size),
            local_pos: (valid_size(local_size).0 / 2, valid_size(local_size).1 / 2),
            remote_pos: (0, 0),
            phase: PointerPhase::Local { armed: true },
            local_buttons_down: 0,
        }
    }

    pub fn is_remote(&self) -> bool {
        matches!(self.phase, PointerPhase::Remote { .. })
    }

    pub fn update_local_size(&mut self, size: (i32, i32)) {
        self.local_size = valid_size(size);
        self.local_pos.0 = self.local_pos.0.clamp(0, self.local_size.0 - 1);
        self.local_pos.1 = self.local_pos.1.clamp(0, self.local_size.1 - 1);
    }

    pub fn calibrate_local_position(&mut self, x: i32, y: i32) {
        if self.is_remote() {
            return;
        }
        self.local_pos = (
            x.clamp(0, self.local_size.0 - 1),
            y.clamp(0, self.local_size.1 - 1),
        );
    }

    pub fn observe_local_delta(&mut self, dx: i32, dy: i32) -> MotionAction {
        self.local_pos.0 = clamp(
            self.local_pos.0.saturating_add(dx),
            0,
            self.local_size.0 - 1,
        );
        self.local_pos.1 = clamp(
            self.local_pos.1.saturating_add(dy),
            0,
            self.local_size.1 - 1,
        );
        self.observe_local_motion(self.local_pos.0, self.local_pos.1)
    }

    pub fn observe_local_motion(&mut self, x: i32, y: i32) -> MotionAction {
        self.local_pos = (
            x.clamp(0, self.local_size.0 - 1),
            y.clamp(0, self.local_size.1 - 1),
        );
        let PointerPhase::Local { armed } = self.phase else {
            return MotionAction::Local;
        };

        if !armed {
            if self.inside_rearm_zone(x) {
                self.phase = PointerPhase::Local { armed: true };
            }
            return MotionAction::Local;
        }
        if self.local_buttons_down != 0 || !self.at_transfer_edge(x) {
            return MotionAction::Local;
        }

        self.remote_pos = match self.edge {
            Edge::Right => (0, scaled(y, self.local_size.1, self.remote_size.1)),
            Edge::Left => (
                self.remote_size.0 - 1,
                scaled(y, self.local_size.1, self.remote_size.1),
            ),
        };
        self.phase = PointerPhase::Remote {
            accumulated_return_push: 0,
            return_armed: false,
        };
        MotionAction::EnterRemote {
            x: self.remote_pos.0,
            y: self.remote_pos.1,
        }
    }

    pub fn observe_remote_motion(&mut self, dx: i32, dy: i32, allow_return: bool) -> MotionAction {
        let PointerPhase::Remote {
            mut accumulated_return_push,
            mut return_armed,
        } = self.phase
        else {
            return MotionAction::Local;
        };

        let moving_toward_local_edge = match self.edge {
            Edge::Right => dx < 0,
            Edge::Left => dx > 0,
        };
        let at_local_edge_before_move = self.is_remote_at_return_edge();

        // Returning requires both being on the return edge and continuing to push
        // toward the local screen, which prevents accidental returns during normal motion.
        if return_armed && allow_return && moving_toward_local_edge && at_local_edge_before_move {
            accumulated_return_push = accumulated_return_push.saturating_add(dx.saturating_abs());
            if accumulated_return_push >= RETURN_PUSH_THRESHOLD {
                return self.return_to_local();
            }
        } else {
            accumulated_return_push = 0;
        }

        self.remote_pos.0 = clamp(
            self.remote_pos.0.saturating_add(dx),
            0,
            self.remote_size.0 - 1,
        );
        self.remote_pos.1 = clamp(
            self.remote_pos.1.saturating_add(dy),
            0,
            self.remote_size.1 - 1,
        );
        let at_local_edge_after_move = self.is_remote_at_return_edge();
        if !at_local_edge_after_move {
            return_armed = true;
        }
        self.phase = PointerPhase::Remote {
            accumulated_return_push,
            return_armed,
        };
        MotionAction::MoveRemote { dx, dy }
    }

    pub fn observe_local_button(&mut self, down: bool) {
        if self.is_remote() {
            return;
        }
        if down {
            self.local_buttons_down = self.local_buttons_down.saturating_add(1);
        } else {
            self.local_buttons_down = self.local_buttons_down.saturating_sub(1);
        }
    }

    pub fn force_local(&mut self) -> Option<(i32, i32)> {
        if !self.is_remote() {
            return None;
        }
        match self.return_to_local() {
            MotionAction::ReturnLocal { x, y } => Some((x, y)),
            _ => None,
        }
    }

    fn return_to_local(&mut self) -> MotionAction {
        let y = scaled(self.remote_pos.1, self.remote_size.1, self.local_size.1);
        let last_x = self.local_size.0.saturating_sub(1);
        let x = match self.edge {
            Edge::Right => last_x.saturating_sub(LOCAL_RETURN_INSET).max(0),
            Edge::Left => LOCAL_RETURN_INSET.min(last_x),
        };
        self.phase = PointerPhase::Local { armed: false };
        self.local_pos = (x, y);
        self.local_buttons_down = 0;
        MotionAction::ReturnLocal { x, y }
    }

    fn at_transfer_edge(&self, x: i32) -> bool {
        match self.edge {
            Edge::Right => x >= self.local_size.0.saturating_sub(EDGE_TRIGGER_MARGIN),
            Edge::Left => x <= EDGE_TRIGGER_MARGIN,
        }
    }

    fn is_remote_at_return_edge(&self) -> bool {
        match self.edge {
            Edge::Right => self.remote_pos.0 <= RETURN_EDGE_MARGIN,
            Edge::Left => {
                self.remote_pos.0 >= self.remote_size.0.saturating_sub(1 + RETURN_EDGE_MARGIN)
            }
        }
    }

    fn inside_rearm_zone(&self, x: i32) -> bool {
        match self.edge {
            Edge::Right => x <= self.local_size.0.saturating_sub(1 + EDGE_REARM_DISTANCE),
            Edge::Left => x >= EDGE_REARM_DISTANCE,
        }
    }
}

fn valid_size(size: (i32, i32)) -> (i32, i32) {
    (size.0.max(1), size.1.max(1))
}

fn scaled(value: i32, from: i32, to: i32) -> i32 {
    clamp(
        (value as i64 * to.max(1) as i64 / from.max(1) as i64) as i32,
        0,
        to.saturating_sub(1),
    )
}

fn clamp(value: i32, min: i32, max: i32) -> i32 {
    value.max(min).min(max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn right_edge_round_trip_has_explicit_enter_move_and_return() {
        let mut router = PointerRouter::new(Edge::Right, (1000, 800), (1200, 900));
        assert_eq!(
            router.observe_local_motion(999, 400),
            MotionAction::EnterRemote { x: 0, y: 450 }
        );
        assert_eq!(
            router.observe_remote_motion(200, 10, true),
            MotionAction::MoveRemote { dx: 200, dy: 10 }
        );
        assert_eq!(
            router.observe_remote_motion(-200, 0, true),
            MotionAction::MoveRemote { dx: -200, dy: 0 }
        );
        assert!(matches!(
            router.observe_remote_motion(-10, 0, true),
            MotionAction::ReturnLocal { x: 991, y: 408 }
        ));
        assert!(!router.is_remote());
    }

    #[test]
    fn return_position_cannot_immediately_reenter() {
        let mut router = PointerRouter::new(Edge::Right, (1000, 800), (1200, 900));
        router.observe_local_motion(999, 400);
        assert_eq!(
            router.observe_remote_motion(30, 0, true),
            MotionAction::MoveRemote { dx: 30, dy: 0 }
        );
        assert_eq!(
            router.observe_remote_motion(-30, 0, true),
            MotionAction::MoveRemote { dx: -30, dy: 0 }
        );
        let MotionAction::ReturnLocal { x, y } = router.observe_remote_motion(-10, 0, true) else {
            panic!("expected return to local");
        };
        assert_eq!(router.observe_local_motion(x, y), MotionAction::Local);
        assert_eq!(
            router.observe_local_motion(999, y),
            MotionAction::EnterRemote { x: 0, y: 450 }
        );
    }

    #[test]
    fn left_edge_round_trip_is_symmetric() {
        let mut router = PointerRouter::new(Edge::Left, (1000, 800), (1200, 900));
        assert_eq!(
            router.observe_local_motion(0, 400),
            MotionAction::EnterRemote { x: 1199, y: 450 }
        );
        assert_eq!(
            router.observe_remote_motion(10, 0, true),
            MotionAction::MoveRemote { dx: 10, dy: 0 }
        );
        assert_eq!(
            router.observe_remote_motion(-30, 0, true),
            MotionAction::MoveRemote { dx: -30, dy: 0 }
        );
        assert_eq!(
            router.observe_remote_motion(30, 0, true),
            MotionAction::MoveRemote { dx: 30, dy: 0 }
        );
        assert_eq!(
            router.observe_remote_motion(10, 0, true),
            MotionAction::ReturnLocal { x: 8, y: 400 }
        );
        assert_eq!(router.observe_local_motion(8, 400), MotionAction::Local);
        assert_eq!(
            router.observe_local_motion(0, 400),
            MotionAction::EnterRemote { x: 1199, y: 450 }
        );
    }

    #[test]
    fn local_button_blocks_transfer_and_remote_button_blocks_return() {
        let mut router = PointerRouter::new(Edge::Right, (1000, 800), (1200, 900));
        router.observe_local_button(true);
        assert_eq!(router.observe_local_motion(999, 400), MotionAction::Local);
        router.observe_local_button(false);
        router.observe_local_motion(999, 400);
        assert_eq!(
            router.observe_remote_motion(64, 0, true),
            MotionAction::MoveRemote { dx: 64, dy: 0 }
        );
        assert_eq!(
            router.observe_remote_motion(-64, 0, false),
            MotionAction::MoveRemote { dx: -64, dy: 0 }
        );
        assert!(router.is_remote());
        assert!(matches!(
            router.observe_remote_motion(-10, 0, true),
            MotionAction::ReturnLocal { .. }
        ));
    }

    #[test]
    fn force_local_is_idempotent() {
        let mut router = PointerRouter::new(Edge::Right, (1000, 800), (1200, 900));
        assert_eq!(router.force_local(), None);
        router.observe_local_motion(999, 400);
        assert_eq!(router.force_local(), Some((991, 400)));
        assert_eq!(router.force_local(), None);
    }
}
