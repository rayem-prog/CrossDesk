//! Local, opt-in diagnostic capture. Never contains input or clipboard values.
use crate::input::latency::{Histogram, Stage};
use std::io;
use std::sync::{
    Condvar, Mutex, OnceLock,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

pub const STAGE_NAMES: [&str; 5] = [
    "send_queue",
    "send_write",
    "receive_queue",
    "inject",
    "pacing_late",
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "gui", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "gui", serde(rename_all = "snake_case"))]
pub enum DiagnosticState {
    #[default]
    Idle,
    Collecting,
    Completed,
    Stopped,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "gui", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "gui", serde(deny_unknown_fields))]
pub struct StageMetrics {
    pub name: String,
    pub count: u64,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub max_us: u64,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "gui", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "gui", serde(deny_unknown_fields))]
pub struct DiagnosticReport {
    pub elapsed_ms: u64,
    pub stages: Vec<StageMetrics>,
    pub coalesced_samples: u64,
    pub max_queue_depth: usize,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "gui", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "gui", serde(deny_unknown_fields))]
pub struct DiagnosticView {
    pub run_id: Option<u64>,
    pub state: DiagnosticState,
    pub remaining_seconds: u64,
    pub report: Option<DiagnosticReport>,
}

#[derive(Default)]
struct Capture {
    connected: bool,
    run: Option<Run>,
    last: DiagnosticView,
}
struct Run {
    id: u64,
    started: Instant,
    histograms: [Histogram; 5],
    coalesced: u64,
    peak: usize,
}
const CAPTURE_TIME: Duration = Duration::from_secs(60);
impl Capture {
    fn open_session(&mut self) {
        *self = Self {
            connected: true,
            ..Self::default()
        };
    }
    fn close_session(&mut self) {
        *self = Self::default();
    }
    fn start(&mut self, id: u64, now: Instant) -> io::Result<()> {
        if !self.connected {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "请先连接，再开始诊断",
            ));
        }
        if id == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid diagnostic run",
            ));
        }
        self.snapshot(now, false);
        if self.run.is_some() {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "诊断正在进行"));
        }
        self.last = DiagnosticView::default();
        self.run = Some(Run {
            id,
            started: now,
            histograms: std::array::from_fn(|_| Histogram::default()),
            coalesced: 0,
            peak: 0,
        });
        Ok(())
    }
    fn record(&mut self, stage: Stage, duration: Duration, now: Instant) {
        if let Some(run) = self
            .run
            .as_mut()
            .filter(|r| now.saturating_duration_since(r.started) < CAPTURE_TIME)
        {
            run.histograms[stage as usize].record(duration);
        }
    }
    fn queue(&mut self, depth: usize, samples: u64, now: Instant) {
        if let Some(run) = self
            .run
            .as_mut()
            .filter(|r| now.saturating_duration_since(r.started) < CAPTURE_TIME)
        {
            run.coalesced = run.coalesced.saturating_add(samples.saturating_sub(1));
            run.peak = run.peak.max(depth);
        }
    }
    fn snapshot(&mut self, now: Instant, stop: bool) -> DiagnosticView {
        let Some(run) = self.run.as_ref() else {
            return self.last.clone();
        };
        let elapsed = now.saturating_duration_since(run.started).min(CAPTURE_TIME);
        let completed = elapsed == CAPTURE_TIME;
        let terminal = stop || completed;
        let view = DiagnosticView {
            run_id: Some(run.id),
            state: if completed {
                DiagnosticState::Completed
            } else if stop {
                DiagnosticState::Stopped
            } else {
                DiagnosticState::Collecting
            },
            remaining_seconds: if terminal {
                0
            } else {
                (CAPTURE_TIME - elapsed).as_millis().div_ceil(1000) as u64
            },
            report: Some(DiagnosticReport {
                elapsed_ms: elapsed.as_millis() as u64,
                stages: run
                    .histograms
                    .iter()
                    .zip(STAGE_NAMES)
                    .map(|(h, n)| h.summary(n))
                    .collect(),
                coalesced_samples: run.coalesced,
                max_queue_depth: run.peak,
            }),
        };
        if terminal {
            self.run = None;
            self.last = view.clone();
        }
        view
    }
}
#[derive(Default)]
struct RuntimeState {
    capture: Capture,
    generation: u64,
}
#[derive(Default)]
struct Runtime {
    state: Mutex<RuntimeState>,
    changed: Condvar,
}
static RUNTIME: OnceLock<Runtime> = OnceLock::new();
static ACTIVE: AtomicU64 = AtomicU64::new(0);
static REPORTER: OnceLock<Result<(), String>> = OnceLock::new();
fn runtime() -> &'static Runtime {
    RUNTIME.get_or_init(Runtime::default)
}
pub fn active_run_id() -> u64 {
    ACTIVE.load(Ordering::Relaxed)
}
pub fn collecting() -> bool {
    active_run_id() != 0
}
pub fn record(stage: Stage, duration: Duration) {
    if collecting() {
        runtime()
            .state
            .lock()
            .unwrap()
            .capture
            .record(stage, duration, Instant::now());
    }
}
pub fn queue(depth: usize, samples: u64) {
    if collecting() {
        runtime()
            .state
            .lock()
            .unwrap()
            .capture
            .queue(depth, samples, Instant::now());
    }
}
pub fn start(id: u64) -> io::Result<()> {
    let rt = runtime();
    let view = {
        let mut state = rt.state.lock().unwrap();
        state.capture.start(id, Instant::now())?;
        ACTIVE.store(id, Ordering::Relaxed);
        state.capture.snapshot(Instant::now(), false)
    };
    emit(&view);
    let reporter = REPORTER.get_or_init(|| {
        std::thread::Builder::new()
            .name("input-diagnostics".into())
            .spawn(|| {
                let rt = runtime();
                loop {
                    let view = {
                        let mut state = rt.state.lock().unwrap();
                        while state.capture.run.is_none() {
                            state = rt.changed.wait(state).unwrap();
                        }
                        let (mut state, _) = rt
                            .changed
                            .wait_timeout(state, Duration::from_secs(1))
                            .unwrap();
                        if state.capture.run.is_none() {
                            continue;
                        }
                        let view = state.capture.snapshot(Instant::now(), false);
                        ACTIVE.store(
                            state.capture.run.as_ref().map_or(0, |run| run.id),
                            Ordering::Relaxed,
                        );
                        view
                    };
                    emit(&view);
                }
            })
            .map(|_| ())
            .map_err(|error| error.to_string())
    });
    if let Err(error) = reporter {
        stop();
        return Err(io::Error::other(format!("无法启动诊断线程：{error}")));
    }
    rt.changed.notify_all();
    Ok(())
}
pub fn stop() {
    let view = {
        let mut state = runtime().state.lock().unwrap();
        let view = state.capture.snapshot(Instant::now(), true);
        ACTIVE.store(0, Ordering::Relaxed);
        view
    };
    runtime().changed.notify_all();
    emit(&view);
}
pub struct SessionGuard(u64);
pub fn begin_session() -> SessionGuard {
    let mut state = runtime().state.lock().unwrap();
    state.generation = state.generation.wrapping_add(1);
    state.capture.open_session();
    ACTIVE.store(0, Ordering::Relaxed);
    SessionGuard(state.generation)
}
impl Drop for SessionGuard {
    fn drop(&mut self) {
        let mut state = runtime().state.lock().unwrap();
        if state.generation == self.0 {
            state.capture.close_session();
            ACTIVE.store(0, Ordering::Relaxed);
            runtime().changed.notify_all();
        }
    }
}
fn emit(view: &DiagnosticView) {
    #[cfg(feature = "gui")]
    if std::env::var_os("DESKBRIDGE_GUI_CHILD").is_some() {
        use std::io::Write;
        let mut out = std::io::stdout().lock();
        let _ = writeln!(
            out,
            "crossdesk-diagnostics\t{}",
            serde_json::to_string(view).unwrap()
        );
        let _ = out.flush();
    }
    #[cfg(not(feature = "gui"))]
    let _ = view;
}
pub fn parse_event(line: &str) -> Option<DiagnosticView> {
    #[cfg(feature = "gui")]
    {
        if line.len() > 8192 {
            return None;
        }
        let view: DiagnosticView =
            serde_json::from_str(line.strip_prefix("crossdesk-diagnostics\t")?).ok()?;
        if view.state == DiagnosticState::Idle {
            return (view == DiagnosticView::default()).then_some(view);
        }
        if view.run_id.is_none_or(|id| id == 0)
            || view.remaining_seconds > 60
            || (view.state != DiagnosticState::Collecting
                && (view.remaining_seconds != 0 || view.report.is_none()))
        {
            return None;
        }
        if let Some(report) = &view.report {
            if report.elapsed_ms > 60_000
                || report.max_queue_depth > 256
                || report.stages.len() != 5
                || (view.state == DiagnosticState::Completed && report.elapsed_ms != 60_000)
            {
                return None;
            }
            for (s, name) in report.stages.iter().zip(STAGE_NAMES) {
                if s.name != name
                    || s.p50_us > s.p95_us
                    || s.p95_us > s.p99_us
                    || (s.count == 0
                        && (s.p50_us != 0 || s.p95_us != 0 || s.p99_us != 0 || s.max_us != 0))
                {
                    return None;
                }
            }
        }
        Some(view)
    }
    #[cfg(not(feature = "gui"))]
    {
        let _ = line;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disabled_capture_has_no_samples_and_start_requires_connection() {
        let mut capture = Capture::default();
        assert!(capture.start(1, Instant::now()).is_err());
        capture.record(Stage::Inject, Duration::from_millis(9), Instant::now());
        assert_eq!(
            capture.snapshot(Instant::now(), false),
            DiagnosticView::default()
        );
    }
    #[test]
    fn capture_expires_at_sixty_seconds_without_a_webview_or_input() {
        let now = Instant::now();
        let mut capture = Capture::default();
        capture.open_session();
        capture.start(7, now).unwrap();
        capture.record(Stage::Inject, Duration::from_micros(17), now);
        capture.record(
            Stage::Inject,
            Duration::from_millis(8),
            now + Duration::from_secs(60),
        );
        capture.queue(9, 4, now);
        let view = capture.snapshot(now + Duration::from_secs(61), false);
        assert_eq!(view.state, DiagnosticState::Completed);
        assert_eq!(view.run_id, Some(7));
        assert_eq!(view.remaining_seconds, 0);
        let report = view.report.unwrap();
        assert_eq!(report.elapsed_ms, 60_000);
        assert_eq!(report.stages[3].count, 1);
        assert_eq!(report.stages[3].p95_us, 25);
        assert_eq!(report.stages[3].max_us, 17);
        assert_eq!(report.coalesced_samples, 3);
        assert_eq!(report.max_queue_depth, 9);
    }
    #[test]
    fn stopping_preserves_report_and_a_new_run_or_session_clears_it() {
        let now = Instant::now();
        let mut capture = Capture::default();
        capture.open_session();
        capture.start(8, now).unwrap();
        capture.record(Stage::SendWrite, Duration::from_millis(1), now);
        let stopped = capture.snapshot(now + Duration::from_secs(2), true);
        assert_eq!(stopped.state, DiagnosticState::Stopped);
        assert_eq!(stopped.report.as_ref().unwrap().elapsed_ms, 2_000);
        assert_eq!(
            capture.snapshot(now + Duration::from_secs(3), false),
            stopped
        );
        capture.start(9, now + Duration::from_secs(4)).unwrap();
        let next = capture.snapshot(now + Duration::from_secs(5), false);
        assert_eq!(next.run_id, Some(9));
        assert_eq!(next.report.unwrap().stages[1].count, 0);
        capture.close_session();
        assert_eq!(capture.snapshot(now, false), DiagnosticView::default());
        assert!(capture.start(10, now).is_err());
    }
    #[test]
    fn duplicate_or_invalid_start_does_not_replace_an_active_run() {
        let now = Instant::now();
        let mut capture = Capture::default();
        capture.open_session();
        assert!(capture.start(0, now).is_err());
        capture.start(1, now).unwrap();
        assert!(capture.start(2, now).is_err());
        assert_eq!(capture.snapshot(now, false).run_id, Some(1));
    }
    #[cfg(feature = "gui")]
    #[test]
    fn events_are_bounded_typed_and_cannot_contain_extra_input_data() {
        let now = Instant::now();
        let mut capture = Capture::default();
        capture.open_session();
        capture.start(55, now).unwrap();
        let view = capture.snapshot(now + Duration::from_secs(60), false);
        let mut json = serde_json::to_value(&view).unwrap();
        let line = format!("crossdesk-diagnostics\t{json}");
        assert_eq!(parse_event(&line), Some(view));
        json["report"]["elapsed_ms"] = 1.into();
        assert!(parse_event(&format!("crossdesk-diagnostics\t{json}")).is_none());
        json["report"]["elapsed_ms"] = 60000.into();
        json["report"]["text"] = "private clipboard".into();
        assert!(parse_event(&format!("crossdesk-diagnostics\t{json}")).is_none());
        assert!(parse_event(&format!("crossdesk-diagnostics\t{}", " ".repeat(8193))).is_none());
        assert!(parse_event("input_metrics inject count=4").is_none());
    }
    #[cfg(feature = "gui")]
    #[test]
    #[ignore = "subprocess fixture driven by runtime_commands_sample_without_native_input"]
    fn diagnostic_runtime_fixture() {
        if std::env::var("CROSSDESK_DIAGNOSTIC_FIXTURE").as_deref() != Ok("1") {
            return;
        }
        let _session = begin_session();
        crate::shutdown::spawn_gui_stop_watcher();
        crate::service_event::emit(crate::service_event::ServiceState::Connected, None);
        while !crate::shutdown::requested() {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    #[cfg(feature = "gui")]
    #[test]
    fn runtime_commands_sample_without_native_input() {
        use std::io::{BufRead, Write};
        use std::process::{Command, Stdio};
        struct ChildGuard(std::process::Child);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut child = ChildGuard(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "diagnostics::tests::diagnostic_runtime_fixture",
                    "--ignored",
                    "--nocapture",
                ])
                .env("DESKBRIDGE_GUI_CHILD", "1")
                .env("CROSSDESK_DIAGNOSTIC_FIXTURE", "1")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let stdout = child.0.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                if tx.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        loop {
            if rx
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .contains("crossdesk-event")
            {
                break;
            }
        }
        let stdin = child.0.stdin.as_mut().unwrap();
        stdin.write_all(b"diagnostics start 73\n").unwrap();
        stdin.flush().unwrap();
        let next = || loop {
            let line = rx.recv_timeout(Duration::from_secs(5)).unwrap();
            if let Some(view) = parse_event(&line) {
                break view;
            }
        };
        let started = next();
        assert_eq!(started.run_id, Some(73));
        assert_eq!(started.state, DiagnosticState::Collecting);
        assert_eq!(
            started
                .report
                .unwrap()
                .stages
                .iter()
                .map(|s| s.count)
                .sum::<u64>(),
            0
        );
        stdin.write_all(b"diagnostics stop\n").unwrap();
        stdin.flush().unwrap();
        let stopped = loop {
            let view = next();
            if view.state == DiagnosticState::Stopped {
                break view;
            }
        };
        assert_eq!(stopped.run_id, Some(73));
        stdin.write_all(b"stop\n").unwrap();
        stdin.flush().unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(
                Instant::now() < until,
                "diagnostic command prevented shutdown"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        reader.join().unwrap();
    }
}
