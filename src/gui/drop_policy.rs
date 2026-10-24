pub fn edge_bounds(
    monitor: (i32, i32, u32, u32),
    scale: f64,
    side: &str,
    expanded: bool,
) -> (i32, i32, u32, u32) {
    let (x, y, width, height) = monitor;
    let w = ((if expanded { 280.0 } else { 24.0 }) * scale).round() as u32;
    let h = ((if expanded { 160.0 } else { 120.0 }) * scale).round() as u32;
    (
        if side == "right" {
            x + width as i32 - w as i32
        } else {
            x
        },
        y + (height as i32 - h as i32) / 2,
        w,
        h,
    )
}
pub fn close_action(state: &str) -> &'static str {
    match state {
        "waiting" => "reject",
        "preparing" | "sending" | "receiving" => "cancel",
        _ => "hide",
    }
}
pub fn should_collapse(now_ms: u64, resized_ms: u64) -> bool {
    now_ms.saturating_sub(resized_ms) > 350
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn geometry_tracks_primary_display_origin_and_scale() {
        assert_eq!(
            edge_bounds((1920, -200, 2560, 1440), 2.0, "right", false),
            (4432, 400, 48, 240)
        );
        assert_eq!(
            edge_bounds((-1920, 0, 1920, 1080), 1.0, "left", true),
            (-1920, 460, 280, 160)
        );
    }
    #[test]
    fn closing_a_transfer_rejects_only_waiting_and_cancels_only_active() {
        assert_eq!(close_action("waiting"), "reject");
        assert_eq!(close_action("preparing"), "cancel");
        assert_eq!(close_action("receiving"), "cancel");
        assert_eq!(close_action("saved"), "hide");
        assert_eq!(close_action("failed"), "hide");
    }
    #[test]
    fn resizing_during_drag_ignores_transient_leave() {
        assert!(!should_collapse(1200, 1000));
        assert!(should_collapse(1600, 1000));
    }
}
