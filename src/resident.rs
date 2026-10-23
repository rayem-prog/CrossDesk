// Desktop residency policy. Native integration lives behind the GUI feature.
pub struct StartupPolicy {
    pending: bool,
    permission_reported: bool,
}
impl StartupPolicy {
    pub fn new(background: bool) -> Self {
        Self {
            pending: background,
            permission_reported: false,
        }
    }
    pub fn reveal(
        &mut self,
        ready: bool,
        paired: bool,
        permission_required: bool,
        connected: bool,
        tray_available: bool,
    ) -> bool {
        let new_permission_error = permission_required && !self.permission_reported;
        self.permission_reported = permission_required;
        if ready && new_permission_error {
            self.pending = false;
            return true;
        }
        if !self.pending {
            return false;
        }
        if !tray_available || (ready && !paired) {
            self.pending = false;
            return true;
        }
        if connected {
            self.pending = false;
        }
        false
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn background_start_reveals_pairing_once_but_not_reconnects() {
        let mut p = StartupPolicy::new(true);
        assert!(!p.reveal(false, false, false, false, true));
        assert!(p.reveal(true, false, false, false, true));
        assert!(!p.reveal(true, true, false, false, true));
    }
    #[test]
    fn new_permission_failure_reveals_once_even_after_a_successful_connection() {
        let mut p = StartupPolicy::new(true);
        assert!(!p.reveal(true, true, false, false, true));
        assert!(p.reveal(true, true, true, false, true));
        let mut p = StartupPolicy::new(true);
        assert!(!p.reveal(true, true, false, true, true));
        assert!(p.reveal(true, true, true, false, true));
        assert!(!p.reveal(true, true, true, false, true));
    }
    #[test]
    fn tray_failure_always_preserves_a_visible_startup() {
        assert!(StartupPolicy::new(true).reveal(false, false, false, false, false));
        assert!(!StartupPolicy::new(false).reveal(true, false, false, false, true));
    }
}
