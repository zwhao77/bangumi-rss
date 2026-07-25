//! Semaphore backed by `crossbeam_channel` — free `try_acquire` + `acquire_timeout`.
//!
//! A bounded channel pre-filled with tokens acts as a counting semaphore:
//! - `recv()` blocks until a token is available.
//! - `send()` on [`Permit::drop`] returns the token, waking a waiter.
//!
//! [`Permit`] is an RAII guard: acquire returns it, dropping it releases the slot.
//! If the semaphore is destroyed while someone is waiting, `acquire` returns `None`.

use crossbeam_channel::{Receiver, Sender, bounded};

/// A counting semaphore with blocking `acquire()` and RAII `Permit`.
pub struct Semaphore {
    rx: Receiver<()>,
    tx: Sender<()>,
}

/// RAII guard: returns a token to the `Semaphore` on drop.
pub struct Permit {
    tx: Sender<()>,
}

impl Semaphore {
    /// Create a new semaphore with `max` permits.
    pub fn new(max: usize) -> Self {
        let (tx, rx) = bounded(max);
        for _ in 0..max {
            tx.send(()).ok();
        }
        Self { rx, tx }
    }

    /// Acquire one permit — blocks until a slot is available.
    /// Returns `None` if the semaphore has been closed.
    pub fn acquire(&self) -> Option<Permit> {
        self.rx.recv().ok().map(|_| Permit {
            tx: self.tx.clone(),
        })
    }

    /// Try to acquire a permit without blocking.
    pub fn try_acquire(&self) -> Option<Permit> {
        self.rx.try_recv().ok().map(|_| Permit {
            tx: self.tx.clone(),
        })
    }

    /// Acquire with a timeout. Returns `None` if no slot becomes available in time.
    pub fn acquire_timeout(&self, dur: std::time::Duration) -> Option<Permit> {
        self.rx.recv_timeout(dur).ok().map(|_| Permit {
            tx: self.tx.clone(),
        })
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.tx.send(()).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn acquire_release() {
        let sem = Semaphore::new(2);
        let p1 = sem.acquire();
        let p2 = sem.acquire();
        drop(p1);
        let _p3 = sem.acquire(); // now one slot is free again
        drop(p2);
    }

    #[test]
    fn concurrency_limit() {
        let sem = Arc::new(Semaphore::new(3));
        let mut handles = Vec::new();
        for _ in 0..10 {
            let s = Arc::clone(&sem);
            handles.push(std::thread::spawn(move || {
                let _p = s.acquire();
                std::thread::sleep(Duration::from_millis(10));
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
    }

    #[test]
    fn try_acquire_works() {
        let sem = Semaphore::new(1);
        let p1 = sem.acquire();
        // All tokens taken; try_acquire should return None.
        assert!(sem.try_acquire().is_none());
        drop(p1);
        // Token returned; try_acquire should succeed.
        assert!(sem.try_acquire().is_some());
    }

    #[test]
    fn acquire_timeout_returns_none() {
        let sem = Semaphore::new(0);
        assert!(sem.acquire_timeout(Duration::from_millis(10)).is_none());
    }
}
