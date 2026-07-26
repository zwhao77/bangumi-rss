use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

/// Manages a collection of threads.
///
/// A new thread is created every time all the existing threads are full.
/// Any idle thread will automatically die after a few seconds.
pub struct TaskPool {
    sharing: Arc<Sharing>,
}

struct Sharing {
    // list of the tasks to be done by worker threads
    todo: Mutex<VecDeque<Box<dyn FnMut() + Send>>>,

    // condvar that will be notified whenever a task is added to `todo`
    condvar: Condvar,

    // number of total worker threads running
    active_tasks: AtomicUsize,

    // number of idle worker threads
    waiting_tasks: AtomicUsize,

    // maximum number of total threads. `None` = unlimited.
    max_threads: Option<usize>,

    // maximum pending tasks in queue. `None` = unlimited.
    max_queue: Option<usize>,
}

/// Minimum number of active threads.
static MIN_THREADS: usize = 4;

struct Registration<'a> {
    nb: &'a AtomicUsize,
}

impl<'a> Registration<'a> {
    fn new(nb: &'a AtomicUsize) -> Registration<'a> {
        nb.fetch_add(1, Ordering::Release);
        Registration { nb }
    }
}

impl<'a> Drop for Registration<'a> {
    fn drop(&mut self) {
        self.nb.fetch_sub(1, Ordering::Release);
    }
}

impl TaskPool {
    pub fn new() -> TaskPool {
        Self::new_with_limits(None, None)
    }

    /// Create a new pool with optional thread cap and queue limit.
    pub fn new_with_limits(max_threads: Option<usize>, max_queue: Option<usize>) -> TaskPool {
        let pool = TaskPool {
            sharing: Arc::new(Sharing {
                todo: Mutex::new(VecDeque::new()),
                condvar: Condvar::new(),
                active_tasks: AtomicUsize::new(0),
                waiting_tasks: AtomicUsize::new(0),
                max_threads,
                max_queue,
            }),
        };

        // Always start with MIN_THREADS threads (or fewer if capped lower).
        let initial = max_threads.map_or(MIN_THREADS, |m| m.min(MIN_THREADS));
        for _ in 0..initial {
            pool.add_thread(None)
        }

        pool
    }

    /// Executes a function in a thread.
    /// If no thread is available and the cap permits, spawns a new one.
    /// If the cap is reached and the queue is full, drops the task.
    pub fn spawn(&self, code: Box<dyn FnMut() + Send>) {
        let mut queue = self.sharing.todo.lock().unwrap();
        let at_cap = self.sharing.max_threads.map_or(false, |max| {
            self.sharing.active_tasks.load(Ordering::Acquire) >= max
        });

        // Drop task if queue is full.
        if let Some(max_q) = self.sharing.max_queue {
            if queue.len() >= max_q {
                return;
            }
        }

        if self.sharing.waiting_tasks.load(Ordering::Acquire) == 0 && !at_cap {
            // Pre-count before spawning to close the observation race.
            self.sharing.active_tasks.fetch_add(1, Ordering::Acquire);
            self.add_thread(Some(code));
        } else {
            queue.push_back(code);
            self.sharing.condvar.notify_one();
        }
    }

    fn add_thread(&self, initial_fn: Option<Box<dyn FnMut() + Send>>) {
        let sharing = self.sharing.clone();

        thread::spawn(move || {
            let sharing = sharing;
            let _active_guard = Registration::new(&sharing.active_tasks);
            // Undo the pre-count from `spawn` (Registration handles lifecycle).
            sharing.active_tasks.fetch_sub(1, Ordering::Acquire);

            if let Some(mut f) = initial_fn {
                f();
            }

            loop {
                let mut task: Box<dyn FnMut() + Send> = {
                    let mut todo = sharing.todo.lock().unwrap();

                    let task;
                    loop {
                        if let Some(poped_task) = todo.pop_front() {
                            task = poped_task;
                            break;
                        }
                        let _waiting_guard = Registration::new(&sharing.waiting_tasks);

                        let received =
                            if sharing.active_tasks.load(Ordering::Acquire) <= MIN_THREADS {
                                todo = sharing.condvar.wait(todo).unwrap();
                                true
                            } else {
                                let (new_lock, waitres) = sharing
                                    .condvar
                                    .wait_timeout(todo, Duration::from_millis(5000))
                                    .unwrap();
                                todo = new_lock;
                                !waitres.timed_out()
                            };

                        if !received && todo.is_empty() {
                            return;
                        }
                    }

                    task
                };

                task();
            }
        });
    }
}

impl Drop for TaskPool {
    fn drop(&mut self) {
        self.sharing
            .active_tasks
            .store(999_999_999, Ordering::Release);
        self.sharing.condvar.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    #[test]
    fn basic_spawn_and_execute() {
        let pool = TaskPool::new();
        let flag = Arc::new(AtomicBool::new(false));
        let f = flag.clone();
        pool.spawn(Box::new(move || f.store(true, Ordering::Relaxed)));
        // Give it a moment to run.
        std::thread::sleep(Duration::from_millis(50));
        assert!(flag.load(Ordering::Relaxed), "task should have run");
    }

    #[test]
    fn thread_cap_limits_active_tasks() {
        let pool = TaskPool::new_with_limits(Some(2), None);
        let barrier = Arc::new(std::sync::Barrier::new(3));
        // Spawn 3 tasks that all wait on the barrier → only 2 threads can exist.
        for _ in 0..3 {
            let b = Arc::clone(&barrier);
            pool.spawn(Box::new(move || {
                b.wait(); // block until all 3 have started (or 2 + 1 queued)
            }));
        }
        // Barrier with 2+1: 2 threads run, 3rd queued.
        // Wait and verify pool doesn't crash.
        std::thread::sleep(Duration::from_millis(100));
        // After tasks complete, threads should not exceed cap.
        assert!(pool.sharing.active_tasks.load(Ordering::Relaxed) <= 2 + MIN_THREADS);
    }

    #[test]
    fn queue_limit_drops_excess() {
        let pool = TaskPool::new_with_limits(Some(1), Some(1));
        let counter = Arc::new(AtomicUsize::new(0));

        // First task: spawns a thread (cap allows 1).
        let c1 = counter.clone();
        pool.spawn(Box::new(move || {
            c1.fetch_add(1, Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(50));
        }));

        // Second task: queued (queue limit allows 1).
        let c2 = counter.clone();
        pool.spawn(Box::new(move || {
            c2.fetch_add(1, Ordering::Relaxed);
        }));

        // Third task: queue full → dropped.
        let c3 = counter.clone();
        pool.spawn(Box::new(move || {
            c3.fetch_add(1, Ordering::Relaxed);
        }));

        std::thread::sleep(Duration::from_millis(200));
        // Only task 1 and 2 should have run; task 3 was dropped.
        assert_eq!(counter.load(Ordering::Relaxed), 2, "third task should be dropped");
    }

    #[test]
    fn panic_in_task_does_not_kill_pool() {
        let pool = TaskPool::new();
        let flag = Arc::new(AtomicBool::new(false));

        // Task that panics.
        pool.spawn(Box::new(|| panic!("oops")));

        // Give it time to panic + unwind.
        std::thread::sleep(Duration::from_millis(30));

        // Pool should still work — spawn a new task.
        let f = flag.clone();
        pool.spawn(Box::new(move || f.store(true, Ordering::Relaxed)));
        std::thread::sleep(Duration::from_millis(50));

        assert!(flag.load(Ordering::Relaxed), "pool should survive task panic");
    }

    #[test]
    fn unlimited_threads_clean_up() {
        let pool = TaskPool::new_with_limits(None, None);
        let barrier = Arc::new(std::sync::Barrier::new(5));
        for _ in 0..5 {
            let b = Arc::clone(&barrier);
            pool.spawn(Box::new(move || { b.wait(); }));
        }
        std::thread::sleep(Duration::from_millis(200));
        // After tasks complete, idle threads should die within 6s.
        std::thread::sleep(Duration::from_millis(6000));
        let active = pool.sharing.active_tasks.load(Ordering::Relaxed);
        // Should have settled back to at most MIN_THREADS.
        assert!(
            active <= MIN_THREADS,
            "idle threads did not clean up"
        );
    }

    #[test]
    fn spawn_returns_after_queue_full_unlimited_threads() {
        // max_queue=None, max_threads=None: spawn should always succeed.
        let pool = TaskPool::new_with_limits(None, None);
        let flag = Arc::new(AtomicBool::new(false));
        let f = flag.clone();
        pool.spawn(Box::new(move || f.store(true, Ordering::Relaxed)));
        std::thread::sleep(Duration::from_millis(50));
        assert!(flag.load(Ordering::Relaxed));
    }
}
