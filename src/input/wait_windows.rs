//! Auto-reset notification plus an interruptible one-shot waitable timer.
use std::io;
use std::ptr;
use std::time::Instant;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_FAILED, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CancelWaitableTimer, CreateEventW,
    CreateWaitableTimerExW, INFINITE, SetEvent, SetWaitableTimer, TIMER_ALL_ACCESS,
    WaitForMultipleObjects,
};

struct OwnedHandle(HANDLE);
// Kernel handles may be waited/signaled from other threads; ownership closes them once.
unsafe impl Send for OwnedHandle {}
unsafe impl Sync for OwnedHandle {}
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

pub(crate) struct WinWakeEvent(OwnedHandle);
impl WinWakeEvent {
    pub(crate) fn new() -> io::Result<Self> {
        let handle = unsafe { CreateEventW(ptr::null(), 0, 0, ptr::null()) };
        if handle.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(OwnedHandle(handle)))
        }
    }
    pub(crate) fn signal(&self) -> io::Result<()> {
        if unsafe { SetEvent(self.0.0) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

pub(crate) struct InputWait(OwnedHandle);
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Wake {
    Event,
    Deadline,
}
impl InputWait {
    pub(crate) fn new() -> io::Result<Self> {
        Self::create(CREATE_WAITABLE_TIMER_HIGH_RESOLUTION).or_else(|_| Self::create(0))
    }
    fn create(flags: u32) -> io::Result<Self> {
        let handle =
            unsafe { CreateWaitableTimerExW(ptr::null(), ptr::null(), flags, TIMER_ALL_ACCESS) };
        if handle.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(OwnedHandle(handle)))
        }
    }
    pub(crate) fn wait(&self, wake: &WinWakeEvent, deadline: Option<Instant>) -> io::Result<Wake> {
        let handles = [wake.0.0, self.0.0];
        let count = if let Some(deadline) = deadline {
            // Negative 100 ns intervals are relative; zero would mean an absolute time.
            let ticks = deadline
                .saturating_duration_since(Instant::now())
                .as_nanos()
                .div_ceil(100)
                .clamp(1, i64::MAX as u128) as i64;
            let relative = -ticks;
            if unsafe { SetWaitableTimer(self.0.0, &relative, 0, None, ptr::null(), 0) } == 0 {
                return Err(io::Error::last_os_error());
            }
            2
        } else {
            unsafe {
                CancelWaitableTimer(self.0.0);
            }
            1
        };
        match unsafe { WaitForMultipleObjects(count, handles.as_ptr(), 0, INFINITE) } {
            WAIT_OBJECT_0 => Ok(Wake::Event),
            value if value == WAIT_OBJECT_0 + 1 => Ok(Wake::Deadline),
            WAIT_FAILED => Err(io::Error::last_os_error()),
            _ => Err(io::Error::other("unexpected input wait result")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    // Dropping a notification or using a manual-reset event breaks the second wait.
    #[test]
    fn notification_is_retained_then_consumed() {
        let wake = WinWakeEvent::new().unwrap();
        let timer = InputWait::new().unwrap();
        wake.signal().unwrap();
        assert_eq!(timer.wait(&wake, None).unwrap(), Wake::Event);
        assert_eq!(
            timer
                .wait(&wake, Some(Instant::now() + Duration::from_millis(2)))
                .unwrap(),
            Wake::Deadline
        );
    }
    // A timer-only sleep would prevent both input and shutdown waking the writer.
    #[test]
    fn notification_interrupts_a_long_timer() {
        let wake = Arc::new(WinWakeEvent::new().unwrap());
        let other = wake.clone();
        let worker = thread::spawn(move || {
            let timer = InputWait::new().unwrap();
            timer
                .wait(&other, Some(Instant::now() + Duration::from_secs(30)))
                .unwrap()
        });
        wake.signal().unwrap();
        assert_eq!(worker.join().unwrap(), Wake::Event);
    }
    #[test]
    fn ordinary_timer_fallback_can_wait_for_deadline() {
        let wake = WinWakeEvent::new().unwrap();
        let timer = InputWait::create(0).unwrap();
        assert_eq!(
            timer.wait(&wake, Some(Instant::now())).unwrap(),
            Wake::Deadline
        );
    }
}
