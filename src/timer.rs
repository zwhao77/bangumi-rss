//! Generic timer manager — single-threaded, zero-dependency.
//!
//! Register `(interval, callback)` pairs and call `run()` to block the
//! current thread, firing each callback on its own cadence.
//!
//! Callbacks return `true` to keep running, `false` to remove themselves.
//! When all timers are removed, the loop exits automatically.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

type Callback = Box<dyn Fn() -> bool + Send + 'static>;

struct Entry {
    interval: Duration,
    next: Instant,
    cb: Callback,
}

/// Holds a set of periodic timers, all driven by a single thread.
pub struct TimerManager {
    entries: Vec<Entry>,
    shutdown: Arc<AtomicBool>,
}

impl TimerManager {
    pub fn new() -> Self {
        Self {
            entries: vec![],
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Returns a handle that can signal the timer thread to stop.
    pub fn shutdown_handle(&self) -> Arc<AtomicBool> {
        self.shutdown.clone()
    }

    /// Register a new periodic timer.  `interval` is the delay between successive
    /// invocations (the first call happens after the first interval elapses).
    ///
    /// The callback must return `true` to keep running, or `false` to cancel
    /// this timer permanently.
    pub fn add(&mut self, interval: Duration, cb: impl Fn() -> bool + Send + 'static) {
        self.entries.push(Entry {
            next: Instant::now() + interval,
            interval,
            cb: Box::new(cb),
        });
    }

    /// Block until `shutdown_handle` is set or all timers cancel themselves.
    pub fn run(mut self) {
        while !self.shutdown.load(Ordering::Relaxed) && !self.entries.is_empty() {
            let now = Instant::now();

            // Fire overdue timers.  Collect indices to remove (in reverse order).
            let mut remove = Vec::new();
            for (i, e) in self.entries.iter_mut().enumerate() {
                if now >= e.next {
                    let keep = (e.cb)();
                    if !keep {
                        remove.push(i);
                    }
                    // Advance to the next scheduled fire.  If the callback blocked
                    // for longer than one interval, skip ahead to catch up.
                    while e.next <= Instant::now() {
                        e.next += e.interval;
                    }
                }
            }

            // Remove stopped timers (reverse order keeps indices valid).
            for i in remove.into_iter().rev() {
                self.entries.swap_remove(i);
            }

            if self.entries.is_empty() {
                break;
            }

            // Sleep until the earliest next fire time.
            // Cap at 1s so shutdown checks are timely — wakes ~86k times/day,
            // consuming < 3ms total CPU (negligible vs a single HTTP request).
            let sleep = self
                .entries
                .iter()
                .map(|e| e.next.saturating_duration_since(Instant::now()))
                .min()
                .unwrap_or(Duration::from_secs(1));
            let sleep = sleep.min(Duration::from_secs(1));
            std::thread::sleep(sleep);
        }
        println!("[timer] shutdown");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn single_timer_fires_and_stops() {
        let hits = Arc::new(Mutex::new(0u32));
        let h = hits.clone();

        let mut tm = TimerManager::new();
        tm.add(Duration::from_millis(5), move || {
            let mut n = h.lock().unwrap();
            if *n >= 4 {
                return false; // stop after 4 fires
            }
            *n += 1;
            true
        });

        std::thread::spawn(move || tm.run());
        std::thread::sleep(Duration::from_millis(100));

        let count = *hits.lock().unwrap();
        assert_eq!(count, 4, "expected exactly 4 fires, got {count}");
    }
}
