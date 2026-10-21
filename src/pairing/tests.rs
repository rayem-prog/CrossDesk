use super::*;
use std::time::{Duration, Instant};
#[test]
fn expired_exhausted_and_concurrent_attempts_never_admit() {
    let now = Instant::now();
    let mut w = PairingWindow::new(now, *b"123456").unwrap();
    for _ in 0..5 {
        assert!(w.admit(now));
        assert!(!w.admit(now));
        w.finish(false);
    }
    assert!(!w.admit(now));
    let mut w = PairingWindow::new(now, *b"123456").unwrap();
    assert!(!w.admit(now + Duration::from_secs(120)));
    let mut w = PairingWindow::new(now, *b"123456").unwrap();
    assert!(w.admit(now));
    w.finish(true);
    assert!(!w.admit(now));
    assert!(PairingWindow::new(now, *b"abcdef").is_err());
}
