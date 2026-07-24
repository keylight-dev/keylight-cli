//! Sleep is injected everywhere the CLI would otherwise block on
//! `std::thread::sleep` (the single 429 retry, confirmation polling), so
//! those code paths are unit-testable without a slow/flaky real-time test.

pub trait Sleeper: Send + Sync {
    fn sleep(&self, duration: std::time::Duration);
}

pub struct RealSleeper;

impl Sleeper for RealSleeper {
    fn sleep(&self, duration: std::time::Duration) {
        std::thread::sleep(duration);
    }
}

#[cfg(test)]
pub struct NoopSleeper {
    pub calls: std::sync::Mutex<Vec<std::time::Duration>>,
}

#[cfg(test)]
impl Default for NoopSleeper {
    fn default() -> Self {
        Self {
            calls: std::sync::Mutex::new(Vec::new()),
        }
    }
}

#[cfg(test)]
impl Sleeper for NoopSleeper {
    fn sleep(&self, duration: std::time::Duration) {
        self.calls.lock().unwrap().push(duration);
    }
}
