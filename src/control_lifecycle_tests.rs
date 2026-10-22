use super::*;
use crate::service_event::{ServiceEvent, ServiceState};
use std::io::BufRead;
#[test]
#[ignore = "backend subprocess fixture only"]
fn child_fixture() {
    if std::env::var_os("CROSSDESK_TEST_CHILD").is_none() {
        return;
    }
    println!("client ready (protocol v6)");
    println!(
        "{}",
        crate::service_event::encode(&ServiceEvent {
            state: ServiceState::Listening,
            message: None
        })
    );
    std::io::stdout().flush().unwrap();
    if std::env::var_os("CROSSDESK_TEST_EXIT").is_some() {
        println!(
            "{}",
            crate::service_event::encode(&ServiceEvent {
                state: ServiceState::Connected,
                message: None
            })
        );
        std::io::stdout().flush().unwrap();
        return;
    }
    for line in std::io::stdin().lock().lines() {
        match line.unwrap().as_str() {
            "retry" => {
                println!(
                    "{}",
                    crate::service_event::encode(&ServiceEvent {
                        state: ServiceState::Retrying,
                        message: None
                    })
                );
                std::io::stdout().flush().unwrap();
            }
            "connect" => {
                println!("crossdesk-capabilities\t{{\"text\":true}}");
                println!(
                    "{}",
                    crate::service_event::encode(&ServiceEvent {
                        state: ServiceState::Connected,
                        message: None
                    })
                );
                std::io::stdout().flush().unwrap();
            }
            "transfer-failed" => {
                use crate::service_event::*;
                for state in [TransferState::Preparing, TransferState::Failed] {
                    println!(
                        "{}",
                        encode_transfer(&TransferEvent {
                            kind: TransferKind::Files,
                            id: "44".repeat(16),
                            direction: TransferDirection::Send,
                            state,
                            message: Some("file unavailable".into())
                        })
                    );
                }
                std::io::stdout().flush().unwrap();
            }
            "stop" => return,
            _ => {}
        }
    }
}
#[test]
fn a_late_connected_event_cannot_revive_an_exited_child() {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args([
        "--exact",
        "control::lifecycle_tests::child_fixture",
        "--ignored",
        "--nocapture",
    ])
    .env("CROSSDESK_TEST_CHILD", "1")
    .env("CROSSDESK_TEST_EXIT", "1");
    let mut b = ProcessBackend::default();
    b.spawn(cmd).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while b.child.as_mut().unwrap().try_wait().unwrap().is_none()
        || b.latest.lock().unwrap().is_none()
    {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    assert!(!b.is_running());
    b.collect_logs();
    assert_eq!(b.state(), ServiceState::Error);
    b.stop().unwrap();
    assert!(b.readers.is_empty());
    b.collect_logs();
    assert_eq!(b.state(), ServiceState::Stopped);
}
fn wait_state(b: &mut ProcessBackend, want: ServiceState) {
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        b.collect_logs();
        if b.state() == want {
            return;
        }
        assert!(
            Instant::now() < until,
            "expected {want:?}, got {:?}",
            b.state()
        );
        thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn a_live_process_is_not_connected_and_repeated_stops_reap_it() {
    for _ in 0..4 {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            "control::lifecycle_tests::child_fixture",
            "--ignored",
            "--nocapture",
        ])
        .env("CROSSDESK_TEST_CHILD", "1");
        let mut b = ProcessBackend::default();
        b.spawn(cmd).unwrap();
        wait_state(&mut b, ServiceState::Listening);
        assert!(b.is_running());
        b.child_stdin
            .as_mut()
            .unwrap()
            .write_all(b"connect\n")
            .unwrap();
        wait_state(&mut b, ServiceState::Connected);
        b.child_stdin
            .as_mut()
            .unwrap()
            .write_all(b"transfer-failed\n")
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            b.collect_logs();
            if b.transfer_status()
                .is_some_and(|e| e.state == crate::service_event::TransferState::Failed)
            {
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(b.state(), ServiceState::Connected);
        b.stop().unwrap();
        assert!(!b.is_running());
        assert!(b.child.is_none());
        assert_eq!(b.state(), ServiceState::Stopped);
        assert!(b.transfer_status().is_none());
    }
}

#[test]
fn negotiated_text_availability_is_cleared_on_retry_stop_and_restart() {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args([
        "--exact",
        "control::lifecycle_tests::child_fixture",
        "--ignored",
        "--nocapture",
    ])
    .env("CROSSDESK_TEST_CHILD", "1");
    let mut b = ProcessBackend::default();
    b.spawn(cmd).unwrap();
    wait_state(&mut b, ServiceState::Listening);
    assert!(b.clipboard_capabilities().is_none());
    b.child_stdin
        .as_mut()
        .unwrap()
        .write_all(b"connect\n")
        .unwrap();
    wait_state(&mut b, ServiceState::Connected);
    assert_eq!(
        b.clipboard_capabilities(),
        Some(crate::service_event::ClipboardCapabilities { text: true })
    );
    b.child_stdin
        .as_mut()
        .unwrap()
        .write_all(b"retry\n")
        .unwrap();
    wait_state(&mut b, ServiceState::Retrying);
    assert!(b.clipboard_capabilities().is_none());
    b.stop().unwrap();
    assert!(b.clipboard_capabilities().is_none());
}
