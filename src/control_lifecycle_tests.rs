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
            command if command.starts_with("diagnostics start ") => {
                let run_id: u64 = command
                    .trim_start_matches("diagnostics start ")
                    .parse()
                    .unwrap();
                println!(
                    "crossdesk-diagnostics\t{{\"run_id\":{run_id},\"state\":\"collecting\",\"remaining_seconds\":59,\"report\":null}}"
                );
                std::io::stdout().flush().unwrap();
            }
            command if command.starts_with("diagnostic-stderr ") => {
                let id: u64 = command
                    .trim_start_matches("diagnostic-stderr ")
                    .parse()
                    .unwrap();
                eprintln!(
                    "crossdesk-diagnostics\t{{\"run_id\":{id},\"state\":\"collecting\",\"remaining_seconds\":1,\"report\":null}}"
                );
                eprintln!("stderr diagnostic fixture done");
                std::io::stderr().flush().unwrap();
            }
            "diagnostics stop" => {
                println!("diagnostic stop received");
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
        .write_all(b"transfer-failed\n")
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while b.transfer_status().is_none() {
        b.collect_logs();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    b.child_stdin
        .as_mut()
        .unwrap()
        .write_all(b"retry\n")
        .unwrap();
    wait_state(&mut b, ServiceState::Retrying);
    assert!(b.clipboard_capabilities().is_none());
    assert!(
        b.transfer_status().is_none(),
        "a disconnected session retained a transfer status"
    );
    b.child_stdin
        .as_mut()
        .unwrap()
        .write_all(b"connect\n")
        .unwrap();
    wait_state(&mut b, ServiceState::Connected);
    assert!(
        b.transfer_status().is_none(),
        "reconnection revived an old transfer"
    );
    b.stop().unwrap();
    assert!(b.clipboard_capabilities().is_none());
}

#[test]
fn structured_diagnostic_events_are_not_human_logs() {
    let (tx, rx) = mpsc::sync_channel(8);
    let line = b"crossdesk-diagnostics\t{\"run_id\":7,\"state\":\"collecting\",\"remaining_seconds\":60,\"report\":null}\n";
    let reader = spawn_log_reader(
        &line[..],
        tx,
        Some(Arc::new(Mutex::new(None))),
        None,
        None,
        None,
    );
    reader.join().unwrap();
    assert!(
        rx.try_recv().is_err(),
        "structured diagnostics leaked into human logs"
    );
}

#[test]
fn diagnostic_start_requires_connection_and_the_child_receives_commands() {
    use crate::diagnostics::DiagnosticState;
    let mut b = ProcessBackend::default();
    assert!(b.start_diagnostics().is_err());
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args([
        "--exact",
        "control::lifecycle_tests::child_fixture",
        "--ignored",
        "--nocapture",
    ])
    .env("CROSSDESK_TEST_CHILD", "1");
    b.spawn(cmd).unwrap();
    wait_state(&mut b, ServiceState::Listening);
    assert!(b.start_diagnostics().is_err());
    b.child_stdin
        .as_mut()
        .unwrap()
        .write_all(b"connect\n")
        .unwrap();
    wait_state(&mut b, ServiceState::Connected);
    let pid = b.child.as_ref().unwrap().id();
    b.start_diagnostics().unwrap();
    let run_id = b.diagnostics().run_id.unwrap();
    assert!(run_id > 0);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        b.collect_logs();
        if b.diagnostics().remaining_seconds == 59 {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(b.diagnostics().state, DiagnosticState::Collecting);
    assert!(b.start_diagnostics().is_err());
    b.stop_diagnostics().unwrap();
    while !b.logs.iter().any(|line| line == "diagnostic stop received") {
        b.collect_logs();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(b.child.as_ref().unwrap().id(), pid);
    b.child_stdin
        .as_mut()
        .unwrap()
        .write_all(b"retry\n")
        .unwrap();
    wait_state(&mut b, ServiceState::Retrying);
    assert_eq!(
        b.diagnostics(),
        crate::diagnostics::DiagnosticView::default()
    );
    b.child_stdin
        .as_mut()
        .unwrap()
        .write_all(b"connect\n")
        .unwrap();
    wait_state(&mut b, ServiceState::Connected);
    b.start_diagnostics().unwrap();
    assert!(b.diagnostics().run_id.unwrap() > run_id);
    b.stop().unwrap();
    assert_eq!(
        b.diagnostics(),
        crate::diagnostics::DiagnosticView::default()
    );
}

#[test]
fn stale_diagnostic_reports_cannot_replace_a_newer_run_or_revive_a_disconnected_run() {
    use crate::diagnostics::{DiagnosticState, DiagnosticView};
    let mut tracker = DiagnosticTracker::default();
    tracker.expected = Some(22);
    tracker.view = DiagnosticView {
        run_id: Some(22),
        state: DiagnosticState::Collecting,
        remaining_seconds: 60,
        report: None,
    };
    let (tx, _rx) = mpsc::sync_channel(8);
    let slot = Arc::new(Mutex::new(tracker));
    let bytes = b"crossdesk-diagnostics\t{\"run_id\":21,\"state\":\"collecting\",\"remaining_seconds\":1,\"report\":null}\n";
    spawn_log_reader(&bytes[..], tx, None, None, None, Some(slot.clone()))
        .join()
        .unwrap();
    assert_eq!(slot.lock().unwrap().view.run_id, Some(22));
    assert_eq!(slot.lock().unwrap().view.remaining_seconds, 60);
}

#[test]
fn diagnostic_send_failure_rolls_back_and_stderr_never_changes_metrics() {
    use crate::diagnostics::{DiagnosticState, DiagnosticView};
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
    b.child_stdin
        .as_mut()
        .unwrap()
        .write_all(b"connect\n")
        .unwrap();
    wait_state(&mut b, ServiceState::Connected);
    let stdin = b.child_stdin.take();
    assert!(b.start_diagnostics().is_err());
    assert_eq!(b.diagnostics(), DiagnosticView::default());
    b.child_stdin = stdin;
    b.start_diagnostics().unwrap();
    let id = b.diagnostics().run_id.unwrap();
    b.child_stdin
        .as_mut()
        .unwrap()
        .write_all(format!("diagnostic-stderr {id}\n").as_bytes())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !b
        .logs
        .iter()
        .any(|line| line == "stderr diagnostic fixture done")
    {
        b.collect_logs();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    assert_ne!(b.diagnostics().remaining_seconds, 1);
    assert_eq!(b.diagnostics().state, DiagnosticState::Collecting);
    b.stop().unwrap();
}

#[test]
fn disconnected_stdout_discards_same_run_updates_even_before_gui_polling() {
    use crate::diagnostics::{DiagnosticState, DiagnosticView};
    let slot = Arc::new(Mutex::new(DiagnosticTracker {
        expected: Some(4),
        view: DiagnosticView {
            run_id: Some(4),
            state: DiagnosticState::Collecting,
            remaining_seconds: 60,
            report: None,
        },
    }));
    let line = format!(
        "{}\ncrossdesk-diagnostics\t{{\"run_id\":4,\"state\":\"collecting\",\"remaining_seconds\":1,\"report\":null}}\n",
        crate::service_event::encode(&ServiceEvent {
            state: ServiceState::Retrying,
            message: None
        })
    );
    let (tx, _rx) = mpsc::sync_channel(8);
    spawn_log_reader(
        std::io::Cursor::new(line.into_bytes()),
        tx,
        Some(Arc::new(Mutex::new(None))),
        None,
        None,
        Some(slot.clone()),
    )
    .join()
    .unwrap();
    assert_eq!(slot.lock().unwrap().view, DiagnosticView::default());
    assert_eq!(slot.lock().unwrap().expected, None);
}

#[test]
fn completed_diagnostics_cannot_be_reopened_by_delayed_collecting_updates() {
    use crate::diagnostics::{
        DiagnosticReport, DiagnosticState, DiagnosticView, STAGE_NAMES, StageMetrics,
    };
    let slot = Arc::new(Mutex::new(DiagnosticTracker {
        expected: Some(15),
        view: DiagnosticView {
            run_id: Some(15),
            state: DiagnosticState::Collecting,
            remaining_seconds: 60,
            report: None,
        },
    }));
    let completed = DiagnosticView {
        run_id: Some(15),
        state: DiagnosticState::Completed,
        remaining_seconds: 0,
        report: Some(DiagnosticReport {
            elapsed_ms: 60_000,
            stages: STAGE_NAMES
                .iter()
                .map(|name| StageMetrics {
                    name: name.to_string(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }),
    };
    let lines = format!(
        "crossdesk-diagnostics\t{}\ncrossdesk-diagnostics\t{{\"run_id\":15,\"state\":\"collecting\",\"remaining_seconds\":1,\"report\":null}}\n",
        serde_json::to_string(&completed).unwrap()
    );
    let (tx, _rx) = mpsc::sync_channel(8);
    spawn_log_reader(
        std::io::Cursor::new(lines.into_bytes()),
        tx,
        None,
        None,
        None,
        Some(slot.clone()),
    )
    .join()
    .unwrap();
    assert_eq!(slot.lock().unwrap().view, completed);
}
