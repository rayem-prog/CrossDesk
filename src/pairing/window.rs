use std::{
    io,
    time::{Duration, Instant},
};
pub const WINDOW: Duration = Duration::from_secs(120);
pub struct PairingWindow {
    code: [u8; 6],
    expires: Instant,
    remaining: u8,
    busy: bool,
    closed: bool,
}
impl PairingWindow {
    pub fn new(now: Instant, code: [u8; 6]) -> io::Result<Self> {
        if !code.iter().all(u8::is_ascii_digit) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "pairing code requires six digits",
            ));
        }
        Ok(Self {
            code,
            expires: now + WINDOW,
            remaining: 5,
            busy: false,
            closed: false,
        })
    }
    pub fn random() -> io::Result<Self> {
        loop {
            let mut b = [0; 4];
            getrandom::getrandom(&mut b).map_err(|e| io::Error::other(e.to_string()))?;
            let n = u32::from_le_bytes(b);
            let bound = u32::MAX - (u32::MAX % 1_000_000);
            if n < bound {
                let s = format!("{:06}", n % 1_000_000);
                return Self::new(Instant::now(), s.as_bytes().try_into().unwrap());
            }
        }
    }
    pub fn admit(&mut self, now: Instant) -> bool {
        if !self.open(now) || self.busy {
            return false;
        }
        self.remaining -= 1;
        self.busy = true;
        true
    }
    pub fn finish(&mut self, success: bool) {
        self.busy = false;
        self.closed |= success;
    }
    pub fn cancel(&mut self) {
        self.closed = true;
        self.code.fill(0);
    }
    pub fn open(&self, now: Instant) -> bool {
        !self.closed && now < self.expires && (self.remaining > 0 || self.busy)
    }
    pub fn code(&self) -> [u8; 6] {
        self.code
    }
    pub fn seconds_left(&self) -> u16 {
        self.expires
            .saturating_duration_since(Instant::now())
            .as_secs() as u16
    }
}
impl Drop for PairingWindow {
    fn drop(&mut self) {
        self.code.fill(0);
    }
}
