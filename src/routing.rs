use crate::server::Edge;

const ENTRY_INSET: i32 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionAction {
    Local,
    EnterRemote { x: i32, y: i32 },
    MoveRemote { dx: i32, dy: i32 },
    ReturnLocal { x: i32, y: i32 },
}

pub struct PointerRouter {
    edges: Edge,
    local_size: (i32, i32),
    remote_size: (i32, i32),
    local_pos: (i32, i32),
    remote_pos: (i32, i32),
    entered_from: Option<Edge>,
    local_buttons_down: usize,
}

impl PointerRouter {
    pub fn new(edges: Edge, local_size: (i32, i32), remote_size: (i32, i32)) -> Self {
        let local_size = valid_size(local_size);
        Self {
            edges,
            local_size,
            remote_size: valid_size(remote_size),
            local_pos: (local_size.0 / 2, local_size.1 / 2),
            remote_pos: (0, 0),
            entered_from: None,
            local_buttons_down: 0,
        }
    }

    pub fn is_remote(&self) -> bool { self.entered_from.is_some() }

    pub fn update_local_size(&mut self, size: (i32, i32)) {
        self.local_size = valid_size(size);
        self.local_pos = bounded_position(self.local_pos, self.local_size);
    }

    pub fn update_remote_size(&mut self, size: (i32, i32)) {
        self.remote_size = valid_size(size);
        self.remote_pos = bounded_position(self.remote_pos, self.remote_size);
    }

    pub fn calibrate_local_position(&mut self, x: i32, y: i32) {
        if !self.is_remote() {
            self.local_pos = (x.clamp(0, self.local_size.0 - 1), y.clamp(0, self.local_size.1 - 1));
        }
    }

    pub fn observe_local_delta(&mut self, dx: i32, dy: i32) -> MotionAction {
        self.observe_local_motion(self.local_pos.0.saturating_add(dx), self.local_pos.1.saturating_add(dy))
    }

    pub fn observe_local_motion(&mut self, x: i32, y: i32) -> MotionAction {
        let previous_x = self.local_pos.0;
        self.local_pos = (x.clamp(0, self.local_size.0 - 1), y.clamp(0, self.local_size.1 - 1));
        if self.is_remote() || self.local_buttons_down != 0 || self.local_size.0 < 2 {
            return MotionAction::Local;
        }
        let side = if x <= 0 && x < previous_x && self.edges.allows(Edge::Left) {
            Edge::Left
        } else if x >= self.local_size.0 - 1 && x > previous_x && self.edges.allows(Edge::Right) {
            Edge::Right
        } else {
            return MotionAction::Local;
        };
        self.remote_pos = (
            match side { Edge::Left => self.remote_size.0 - 1 - inset(self.remote_size.0), _ => inset(self.remote_size.0) },
            scaled(y, self.local_size.1, self.remote_size.1),
        );
        self.entered_from = Some(side);
        MotionAction::EnterRemote { x: self.remote_pos.0, y: self.remote_pos.1 }
    }

    pub fn observe_remote_motion(&mut self, dx: i32, dy: i32, allow_return: bool) -> MotionAction {
        if !self.is_remote() { return MotionAction::Local; }
        let next_x = self.remote_pos.0 as i64 + dx as i64;
        self.remote_pos.1 = self.remote_pos.1.saturating_add(dy).clamp(0, self.remote_size.1 - 1);
        // A Mac left exit connects to Windows right, and vice versa. Testing the
        // proposed coordinate lets a single fast motion cross a boundary.
        if allow_return && dx < 0 && next_x <= 0 && self.edges.allows(Edge::Right) {
            return self.return_to_local(Edge::Right);
        }
        if allow_return && dx > 0 && next_x >= (self.remote_size.0 - 1) as i64 && self.edges.allows(Edge::Left) {
            return self.return_to_local(Edge::Left);
        }
        self.remote_pos.0 = next_x.clamp(0, (self.remote_size.0 - 1) as i64) as i32;
        MotionAction::MoveRemote { dx, dy }
    }

    pub fn observe_local_button(&mut self, down: bool) {
        if !self.is_remote() {
            self.local_buttons_down = if down { self.local_buttons_down.saturating_add(1) } else { self.local_buttons_down.saturating_sub(1) };
        }
    }

    pub fn force_local(&mut self) -> Option<(i32, i32)> {
        let side = self.entered_from?;
        match self.return_to_local(side) {
            MotionAction::ReturnLocal { x, y } => Some((x, y)),
            _ => unreachable!(),
        }
    }

    fn return_to_local(&mut self, side: Edge) -> MotionAction {
        self.local_pos = (
            if side == Edge::Left { inset(self.local_size.0) } else { self.local_size.0 - 1 - inset(self.local_size.0) },
            scaled(self.remote_pos.1, self.remote_size.1, self.local_size.1),
        );
        self.entered_from = None;
        self.local_buttons_down = 0;
        MotionAction::ReturnLocal { x: self.local_pos.0, y: self.local_pos.1 }
    }
}

fn valid_size(size: (i32, i32)) -> (i32, i32) { (size.0.clamp(1, 32768), size.1.clamp(1, 32768)) }
fn bounded_position(position: (i32, i32), size: (i32, i32)) -> (i32, i32) {
    (position.0.clamp(0, size.0 - 1), position.1.clamp(0, size.1 - 1))
}
fn inset(width: i32) -> i32 { ENTRY_INSET.min((width - 1) / 4) }
fn scaled(value: i32, from: i32, to: i32) -> i32 {
    if from <= 1 || to <= 1 { return 0; }
    ((value.clamp(0, from - 1) as i64 * (to - 1) as i64 + (from - 1) as i64 / 2) / (from - 1) as i64) as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    fn both() -> PointerRouter { PointerRouter::new(Edge::Both, (1000, 800), (1200, 900)) }

    #[test]
    fn resizing_remote_preserves_proportional_entry_and_return() {
        let mut r = both();
        r.update_remote_size((1600, 1000));
        assert_eq!(r.observe_local_motion(999, 799), MotionAction::EnterRemote { x: 8, y: 999 });
        assert_eq!(r.observe_remote_motion(-8, -999, true), MotionAction::ReturnLocal { x: 991, y: 0 });
    }
    #[test]
    fn resizing_local_preserves_proportional_entry_and_return() {
        let mut r = both();
        r.update_local_size((2000, 1600));
        assert_eq!(r.observe_local_motion(1999, 1599), MotionAction::EnterRemote { x: 8, y: 899 });
        assert_eq!(r.observe_remote_motion(-8, -899, true), MotionAction::ReturnLocal { x: 1991, y: 0 });
    }
    #[test]
    fn resizing_clamps_existing_positions_and_unsafe_dimensions() {
        let mut r = both();
        r.observe_local_motion(0, 799);
        r.update_remote_size((2, 2));
        assert_eq!(r.remote_pos, (1, 1));
        r.update_remote_size((i32::MAX, i32::MIN));
        assert_eq!(r.remote_size, (32768, 1));
        assert_eq!(r.remote_pos, (1, 0));
        r.force_local();
        r.update_local_size((2, 2));
        assert_eq!(r.local_pos, (1, 0));
    }
    #[test]
    fn windows_left_enters_mac_right_and_mac_right_returns_windows_left() {
        let mut r = both();
        assert_eq!(r.observe_local_motion(0, 400), MotionAction::EnterRemote { x: 1191, y: 450 });
        assert_eq!(r.observe_remote_motion(9, 0, true), MotionAction::ReturnLocal { x: 8, y: 400 });
    }
    #[test]
    fn windows_right_enters_mac_left_and_mac_left_returns_windows_right() {
        let mut r = both();
        assert_eq!(r.observe_local_motion(999, 400), MotionAction::EnterRemote { x: 8, y: 450 });
        assert_eq!(r.observe_remote_motion(-9, 0, true), MotionAction::ReturnLocal { x: 991, y: 400 });
    }
    #[test]
    fn can_traverse_mac_and_exit_opposite_edge_in_one_fast_motion() {
        let mut r = both();
        r.observe_local_motion(999, 799);
        assert_eq!(r.observe_remote_motion(2000, 0, true), MotionAction::ReturnLocal { x: 8, y: 799 });
        assert_eq!(r.observe_local_motion(0, 799), MotionAction::EnterRemote { x: 1191, y: 899 });
        assert_eq!(r.observe_remote_motion(-2000, -899, true), MotionAction::ReturnLocal { x: 991, y: 0 });
    }
    #[test]
    fn internal_screen_seam_is_not_a_transfer_edge() {
        let mut r = PointerRouter::new(Edge::Both, (3840, 1080), (1728, 1117));
        assert_eq!(r.observe_local_motion(1919, 500), MotionAction::Local);
        assert_eq!(r.observe_local_motion(1920, 500), MotionAction::Local);
        assert!(!r.is_remote());
    }
    #[test]
    fn no_dwell_or_six_pixel_early_switch_and_no_immediate_bounce() {
        let mut r = both();
        assert_eq!(r.observe_local_motion(998, 400), MotionAction::Local);
        assert!(matches!(r.observe_local_motion(999, 400), MotionAction::EnterRemote { .. }));
        assert_eq!(r.observe_remote_motion(0, 5, true), MotionAction::MoveRemote { dx: 0, dy: 5 });
        assert!(matches!(r.observe_remote_motion(-8, 0, true), MotionAction::ReturnLocal { .. }));
        assert_eq!(r.observe_local_motion(991, 400), MotionAction::Local);
    }
    #[test]
    fn dragging_stays_on_its_current_machine_until_button_release() {
        let mut r = both();
        r.observe_local_button(true);
        assert_eq!(r.observe_local_motion(999, 100), MotionAction::Local);
        r.observe_local_button(false);
        r.observe_local_motion(900, 100);
        r.observe_local_motion(999, 100);
        assert!(matches!(r.observe_remote_motion(-100, 0, false), MotionAction::MoveRemote { .. }));
        assert!(r.is_remote());
        assert!(matches!(r.observe_remote_motion(-1, 0, true), MotionAction::ReturnLocal { .. }));
    }
    #[test]
    fn emergency_release_is_idempotent_and_restores_the_entry_side() {
        let mut r = both();
        assert_eq!(r.force_local(), None);
        r.observe_local_motion(999, 0);
        r.observe_remote_motion(600, 0, true);
        assert_eq!(r.force_local(), Some((991, 0)));
        assert_eq!(r.force_local(), None);
    }
    #[test]
    fn single_edge_configuration_is_preserved() {
        let mut r = PointerRouter::new(Edge::Right, (1000, 800), (1200, 900));
        assert_eq!(r.observe_local_motion(0, 0), MotionAction::Local);
        r.observe_local_motion(999, 0);
        assert!(matches!(r.observe_remote_motion(2000, 0, true), MotionAction::MoveRemote { .. }));
        assert!(matches!(r.observe_remote_motion(-2000, 0, true), MotionAction::ReturnLocal { .. }));
    }
    #[test]
    fn coordinates_are_bounded_for_small_screens_and_extreme_deltas() {
        let mut r = PointerRouter::new(Edge::Both, (2, 2), (2, 2));
        assert_eq!(r.observe_local_motion(0, 1), MotionAction::EnterRemote { x: 1, y: 1 });
        assert_eq!(r.observe_remote_motion(i32::MAX, i32::MIN, true), MotionAction::ReturnLocal { x: 0, y: 0 });
        assert_eq!(scaled(799, 800, 900), 899);
        assert_eq!(scaled(0, 1, 1), 0);
    }
}
