//! Opt-in loopback network-stage comparison; never injects OS input or measures screen latency.
use super::*;
use crate::protocol::InputEvent;
use std::io::Write;

const SAMPLES: usize = 2_000;

fn encrypted_pair() -> (SecureStream, SecureStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        SecureStream::accept(listener.accept().unwrap().0, &[83; 32]).unwrap()
    });
    let socket = TcpStream::connect(endpoint).unwrap();
    let control = socket.try_clone().unwrap();
    let client = crate::secure::from_tcp(socket, &[83; 32], true).unwrap();
    (client, server.join().unwrap(), control)
}

fn event(sequence: usize) -> InputEvent {
    let delta = sequence as i32 + 1;
    InputEvent::MouseDelta {
        dx: delta,
        dy: -delta,
    }
}

struct Metrics {
    write: Vec<u128>,
    roundtrip: Vec<u128>,
}
fn micros(duration: Duration) -> u128 {
    duration.as_nanos().div_ceil(1_000)
}
fn summary(mut samples: Vec<u128>) -> String {
    samples.sort_unstable();
    let percentile = |percent: usize| samples[(samples.len() * percent).div_ceil(100) - 1];
    format!(
        "{{\"samples\":{},\"p50_us\":{},\"p95_us\":{},\"p99_us\":{},\"max_us\":{}}}",
        samples.len(),
        percentile(50),
        percentile(95),
        percentile(99),
        samples.last().unwrap()
    )
}
fn measure_primary() -> Metrics {
    let (mut sender, mut receiver, _control) = encrypted_pair();
    let receiving = thread::spawn(move || {
        let writer = SharedWriter::new(receiver.try_clone().unwrap());
        for sequence in 0..SAMPLES {
            let frame = protocol::read_frame(&mut receiver).unwrap();
            assert_eq!(frame.kind, FrameKind::Input);
            assert_eq!(
                protocol::decode_input(&frame.payload).unwrap(),
                event(sequence)
            );
            writer.write(frame).unwrap();
        }
    });
    let sending = thread::spawn(move || {
        let writer = SharedWriter::new(sender.try_clone().unwrap());
        let mut metrics = Metrics {
            write: Vec::with_capacity(SAMPLES),
            roundtrip: Vec::with_capacity(SAMPLES),
        };
        for sequence in 0..SAMPLES {
            let input = event(sequence);
            let frame = Frame::new(FrameKind::Input, protocol::encode_input(&input));
            let started = Instant::now();
            // The same complete-frame SharedWriter path used by primary input.
            writer.write(frame).unwrap();
            metrics.write.push(micros(started.elapsed()));
            let echo = protocol::read_frame(&mut sender).unwrap();
            metrics.roundtrip.push(micros(started.elapsed()));
            assert_eq!(echo.kind, FrameKind::Input);
            assert_eq!(protocol::decode_input(&echo.payload).unwrap(), input);
        }
        metrics
    });
    let metrics = sending.join().unwrap();
    receiving.join().unwrap();
    metrics
}

#[test]
#[ignore = "opt-in loopback network comparison; no OS injection or screen latency"]
fn compare_primary_network_latency_with_idle_and_blocked_edge_lane() {
    let (idle_lane, idle_peer, _idle_control) = encrypted_pair();
    let idle = measure_primary();
    drop((idle_lane, idle_peer));

    let (mut lane, lane_peer, control) = encrypted_pair();
    // Applied after Noise establishes its standard timeout. Socket clones share SO_SNDTIMEO.
    control
        .set_write_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let cancel = stop.clone();
    let (blocked_tx, blocked_rx) = mpsc::sync_channel(1);
    let load = thread::spawn(move || {
        let payload = [17; 64 * 1024];
        let mut queued = 0u64;
        let mut confirmed = false;
        while !cancel.load(Ordering::Acquire) {
            match lane.write_all(&payload) {
                Ok(()) => queued += payload.len() as u64,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) =>
                {
                    if !confirmed {
                        // Proof comes AFTER an actual pending write times out against the full
                        // send buffer. The peer never drains data; subsequent writes stay blocked.
                        blocked_tx.send(queued).unwrap();
                        confirmed = true;
                    }
                }
                Err(_) if cancel.load(Ordering::Acquire) => break,
                Err(error) => panic!("unexpected auxiliary load failure: {error}"),
            }
        }
        assert!(confirmed, "auxiliary send buffer was never saturated");
    });
    let queued = blocked_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("load did not reach a blocked socket write");
    assert!(
        queued > 0,
        "expected actual auxiliary bytes before write backpressure"
    );
    assert!(!load.is_finished());
    let blocked = measure_primary();
    stop.store(true, Ordering::Release);
    control.shutdown(Shutdown::Both).unwrap();
    load.join().unwrap();
    drop(lane_peer);

    println!(
        "{{\"measurement\":\"loopback_network_stages_only\",\"input_order_preserved\":true,\"blocked_lane_confirmed\":true,\"lane_bytes_queued_before_timeout\":{},\"idle_lane\":{{\"primary_write\":{},\"roundtrip\":{}}},\"blocked_lane\":{{\"primary_write\":{},\"roundtrip\":{}}}}}",
        queued,
        summary(idle.write),
        summary(idle.roundtrip),
        summary(blocked.write),
        summary(blocked.roundtrip)
    );
}
