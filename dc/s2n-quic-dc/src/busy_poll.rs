// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

use crate::tracing::*;
use parking_lot::{Condvar, Mutex};
use std::{
    fmt,
    future::Future,
    ops,
    panic::Location,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU64, AtomicUsize, Ordering},
        Arc, Weak,
    },
    task::Context,
    time::Instant,
};

pub mod clock;
#[cfg(target_os = "linux")]
pub mod thread_dump;

#[derive(Clone)]
pub struct Pool {
    handles: Arc<[Handle]>,
}

impl fmt::Debug for Pool {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Pool").finish_non_exhaustive()
    }
}

/// Per-worker heartbeat state monitored by the watchdog thread.
pub struct Heartbeat {
    /// Bumped after each full task-list iteration. Watchdog compares snapshots to detect stalls.
    counter: AtomicU64,
    /// Index of the task currently being polled (-1 = between tasks).
    current_task: AtomicI64,
    /// Linux thread ID (set by the worker on startup). Used for signal-based thread dumps.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    tid: AtomicI32,
    sleeping: AtomicBool,
}

impl Heartbeat {
    fn new() -> Self {
        Self {
            counter: AtomicU64::new(0),
            current_task: AtomicI64::new(-1),
            tid: AtomicI32::new(0),
            sleeping: AtomicBool::new(false),
        }
    }
}

impl Pool {
    pub fn new(handles: Arc<[Handle]>) -> Self {
        Self { handles }
    }

    /// Spawns a watchdog thread that monitors all workers for stalls.
    ///
    /// If any worker's heartbeat counter doesn't advance within `timeout`,
    /// the watchdog prints which worker and task index is stuck, then aborts.
    pub fn spawn_watchdog(&self, timeout: std::time::Duration) {
        let heartbeats: Vec<Arc<Heartbeat>> =
            self.handles.iter().map(|h| h.heartbeat.clone()).collect();
        std::thread::Builder::new()
            .name("busy_poll_watchdog".into())
            .spawn(move || {
                let mut prev: Vec<u64> = vec![0; heartbeats.len()];
                loop {
                    std::thread::sleep(timeout);
                    for (worker_id, hb) in heartbeats.iter().enumerate() {
                        let current = hb.counter.load(Ordering::Relaxed);
                        let sleeping = hb.sleeping.load(Ordering::Acquire);
                        if current == prev[worker_id] && current > 0 && !sleeping {
                            let task_idx = hb.current_task.load(Ordering::Relaxed);
                            error!(
                                "[watchdog] worker {worker_id} stuck in task {task_idx} \
                                 (heartbeat={current}, no progress in {timeout:?})"
                            );

                            #[cfg(target_os = "linux")]
                            {
                                let tid = hb.tid.load(Ordering::Acquire);
                                let timeout = std::time::Duration::from_secs(10);
                                if let Some(bt) = thread_dump::dump(tid, timeout) {
                                    error!(
                                        "[watchdog] worker {worker_id} (tid={tid}) backtrace:\n{bt}"
                                    );
                                } else {
                                    error!(
                                        "[watchdog] worker {worker_id} (tid={tid}) \
                                         did not respond to dump request within {timeout:?}"
                                    );
                                }
                            }

                            eprintln!(
                                "[watchdog] process alive for debugger attach: pid={}",
                                std::process::id()
                            );
                            std::thread::sleep(std::time::Duration::from_secs(10));

                            std::process::abort();
                        }
                        prev[worker_id] = current;
                    }
                }
            })
            .expect("failed to spawn watchdog thread");
    }

    /// Registers the pool's load gauges on `registry`, so a reporter can tell how busy a pool is
    /// whose workers spin whether or not they have work (CPU time says nothing about it):
    ///
    /// - `busy_poll.pass_rate`, one variant per worker named by `label(worker_id)`: full
    ///   task-list passes per second over the reporter's interval. It sits at its ceiling while
    ///   every task returns `Pending` at once and falls as tasks spend time doing work, so it is
    ///   an inverse load signal. The first sample after registration has no baseline and reports
    ///   0, which the reporter suppresses, as it does for a parked worker.
    /// - `busy_poll.workers`, variants `spinning` and `sleeping`: how many workers are in the spin
    ///   loop and how many are parked with no tasks.
    ///
    /// The values are computed in the reporter's callbacks, off the worker threads. The only cost
    /// on the hot path is the heartbeat increment the watchdog already needs. The callbacks hold
    /// each worker's heartbeat, not a [`Handle`], so a registered pool still shuts down when its
    /// last handle drops.
    pub fn register_load_gauges(
        &self,
        registry: &crate::counter::Registry,
        label: impl Fn(usize) -> String,
    ) {
        for handle in self.handles.iter() {
            let heartbeat = handle.heartbeat.clone();
            let mut prev: Option<(u64, Instant)> = None;
            registry.register_nominal_gauge_callback(
                "busy_poll.pass_rate",
                label(handle.worker_id()),
                move || {
                    let now = Instant::now();
                    let passes = heartbeat.counter.load(Ordering::Relaxed);
                    let rate = match prev {
                        Some((prev_passes, prev_at)) => {
                            let secs = now.duration_since(prev_at).as_secs_f64();
                            if secs > 0.0 {
                                (passes.wrapping_sub(prev_passes) as f64 / secs) as i64
                            } else {
                                0
                            }
                        }
                        None => 0,
                    };
                    prev = Some((passes, now));
                    rate
                },
            );
        }
        let heartbeats: Arc<[Arc<Heartbeat>]> =
            self.handles.iter().map(|h| h.heartbeat.clone()).collect();
        for (variant, sleeping) in [("spinning", false), ("sleeping", true)] {
            let heartbeats = heartbeats.clone();
            registry.register_nominal_gauge_callback("busy_poll.workers", variant, move || {
                heartbeats
                    .iter()
                    .filter(|hb| hb.sleeping.load(Ordering::Acquire) == sleeping)
                    .count() as i64
            });
        }
    }
}

impl<T> From<T> for Pool
where
    Arc<[Handle]>: From<T>,
{
    fn from(handles: T) -> Self {
        Self::new(Arc::from(handles))
    }
}

impl ops::Deref for Pool {
    type Target = [Handle];

    fn deref(&self) -> &Self::Target {
        &self.handles
    }
}

pub struct Handle {
    shared: Arc<Shared>,
    heartbeat: Arc<Heartbeat>,
    worker_id: usize,
}

impl Handle {
    pub fn new(worker_id: usize) -> (Self, Runner) {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                spawns: Vec::with_capacity(16),
                closed: false,
            }),
            spawn_cv: Condvar::new(),
            handles: AtomicUsize::new(1),
        });
        let heartbeat = Arc::new(Heartbeat::new());
        let handle = Self {
            shared: shared.clone(),
            heartbeat: heartbeat.clone(),
            worker_id,
        };
        let runner = Runner {
            shared: Arc::downgrade(&shared),
            heartbeat,
            worker_id,
        };
        (handle, runner)
    }

    /// The worker slot this handle drives, as passed to [`Handle::new`].
    pub fn worker_id(&self) -> usize {
        self.worker_id
    }

    /// Full task-list passes the worker has completed so far. Bumped once per pass over every
    /// task, so on an idle worker it climbs at the empty-pass rate and slows as tasks spend time
    /// in `poll`. Monotonic, wrapping at `u64::MAX`.
    pub fn passes(&self) -> u64 {
        self.heartbeat.counter.load(Ordering::Relaxed)
    }

    /// Whether the worker is parked on the spawn condvar with no tasks, rather than spinning.
    pub fn is_sleeping(&self) -> bool {
        self.heartbeat.sleeping.load(Ordering::Acquire)
    }

    #[track_caller]
    pub fn spawn<F>(&self, task: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.spawn_with_priority(task, None);
    }

    #[track_caller]
    pub fn spawn_with_priority<F>(&self, task: F, priority: Option<u8>)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.spawn_local(move |mut spawner| {
            spawner.spawn_with_priority(task, priority);
        });
    }

    /// Spawns a non-Send future by passing a Send function that will be called
    /// on the runner's thread to produce the future.
    ///
    /// This allows creating !Send futures (e.g. using `Rc`-based channels)
    /// that are polled entirely on the busy-poll thread.
    pub fn spawn_local<F>(&self, f: F)
    where
        F: FnOnce(Spawner) + Send + 'static,
    {
        self.shared.state.lock().spawns.push(Spawn::new(f));
        self.shared.spawn_cv.notify_one();
    }
}

impl Clone for Handle {
    fn clone(&self) -> Self {
        self.shared.handles.fetch_add(1, Ordering::AcqRel);
        Self {
            shared: self.shared.clone(),
            heartbeat: self.heartbeat.clone(),
            worker_id: self.worker_id,
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if self.shared.handles.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.shared.state.lock().closed = true;
            self.shared.spawn_cv.notify_all();
        }
    }
}

pub struct Spawner<'a> {
    tasks: &'a mut Tasks,
    pub(crate) worker_id: usize,
}

impl<'a> Spawner<'a> {
    #[track_caller]
    pub fn spawn<F>(&mut self, future: F)
    where
        F: Future<Output = ()> + 'static,
    {
        self.spawn_with_priority(future, None);
    }

    #[track_caller]
    pub fn spawn_with_priority<F>(&mut self, future: F, priority: Option<u8>)
    where
        F: Future<Output = ()> + 'static,
    {
        self.spawn_with_priority_and_name(future, priority, None);
    }

    #[track_caller]
    pub fn spawn_with_priority_and_name<F>(
        &mut self,
        future: F,
        priority: Option<u8>,
        name: Option<String>,
    ) where
        F: Future<Output = ()> + 'static,
    {
        let priority = priority.unwrap_or(128);
        let task = Task {
            task: Box::pin(future),
            priority,
            location: Location::caller(),
            name,
        };

        self.tasks.spawn(task);
    }
}

struct State {
    spawns: Vec<Spawn>,
    closed: bool,
}

struct Shared {
    state: Mutex<State>,
    spawn_cv: Condvar,
    handles: AtomicUsize,
}

/// A `Send` factory that produces a (possibly `!Send`) `Task` on the runner thread.
struct Spawn {
    factory: Box<dyn FnOnce(Spawner) + Send>,
}

impl Spawn {
    fn new(f: impl FnOnce(Spawner) + Send + 'static) -> Self {
        Self {
            factory: Box::new(f),
        }
    }

    fn into_tasks(self, tasks: &mut Tasks, worker_id: usize) {
        (self.factory)(Spawner { tasks, worker_id })
    }
}

/// A task that lives exclusively on the runner thread. May be `!Send`.
struct Task {
    task: Pin<Box<dyn Future<Output = ()> + 'static>>,
    priority: u8,
    #[allow(dead_code)]
    location: &'static Location<'static>,
    #[allow(dead_code)]
    name: Option<String>,
}

#[must_use]
pub struct Runner {
    shared: Weak<Shared>,
    heartbeat: Arc<Heartbeat>,
    worker_id: usize,
}

impl Runner {
    pub fn run(self) {
        let shared = self.shared;
        let heartbeat = self.heartbeat;
        let worker_id = self.worker_id;

        #[cfg(target_os = "linux")]
        {
            thread_dump::install_handler();
            let tid = unsafe { libc::syscall(libc::SYS_gettid) } as i32;
            heartbeat.tid.store(tid, Ordering::Release);
        }

        let waker = s2n_quic_core::task::waker::noop();
        let mut cx = Context::from_waker(&waker);
        let mut tasks = Tasks::new();
        let mut spawns = Vec::with_capacity(16);

        struct AbortOnPanic;

        impl Drop for AbortOnPanic {
            fn drop(&mut self) {
                if std::thread::panicking() {
                    std::process::abort();
                }
            }
        }

        let _guard = AbortOnPanic;

        loop {
            const ITERATIONS: usize = if cfg!(debug_assertions) {
                10
            } else {
                1_000_000
            };

            let Some(shared) = shared.upgrade() else {
                return;
            };
            if tasks.is_empty() {
                heartbeat.sleeping.store(true, Ordering::Release);
                let mut guard = shared.state.lock();
                while guard.spawns.is_empty() {
                    if guard.closed {
                        return;
                    }
                    shared.spawn_cv.wait(&mut guard);
                }
                heartbeat.sleeping.store(false, Ordering::Release);
                core::mem::swap(&mut spawns, &mut guard.spawns);
            } else {
                for _ in 0..ITERATIONS {
                    tasks.poll(&mut cx, &heartbeat);
                }

                // Yield to allow other threads (especially SCHED_OTHER threads like Tokio runtime)
                // to make progress when running with RT scheduling
                #[cfg(target_os = "linux")]
                unsafe {
                    libc::sched_yield();
                }

                if let Some(mut guard) = shared.state.try_lock() {
                    core::mem::swap(&mut spawns, &mut guard.spawns);
                }
            }

            if spawns.is_empty() {
                continue;
            }

            for spawn in spawns.drain(..) {
                spawn.into_tasks(&mut tasks, worker_id);
            }

            tasks.after_spawn();
        }
    }
}

struct Tasks {
    slots: Vec<Option<Task>>,
    free: Vec<usize>,
    active: usize,
}

impl Tasks {
    const fn new() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            active: 0,
        }
    }

    fn spawn(&mut self, task: Task) {
        if let Some(idx) = self.free.pop() {
            self.slots[idx] = Some(task);
        } else {
            self.slots.push(Some(task));
        }
        self.active += 1;
    }

    fn is_empty(&self) -> bool {
        self.active == 0
    }

    fn after_spawn(&mut self) {
        self.free.clear();

        self.slots.sort_by(|a, b| {
            match (a, b) {
                // priority 0 is highest
                (Some(a), Some(b)) => a.priority.cmp(&b.priority),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
        });

        // clear out the free slots
        while self.slots.last().is_some_and(Option::is_none) {
            let slot = self.slots.pop().unwrap();
            debug_assert!(slot.is_none());
        }
    }

    fn poll(&mut self, cx: &mut Context, heartbeat: &Heartbeat) {
        for (idx, slot) in self.slots.iter_mut().enumerate() {
            if let Some(task) = slot {
                heartbeat.current_task.store(idx as i64, Ordering::Relaxed);
                if task.task.as_mut().poll(cx).is_ready() {
                    eprintln!("task {idx} done ({})", task.location);
                    *slot = None;
                    self.free.push(idx);
                    self.active -= 1;
                }
            }
        }
        heartbeat.current_task.store(-1, Ordering::Relaxed);
        heartbeat.counter.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };

    fn wait_until(timeout: Duration, mut pred: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if pred() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        pred()
    }

    #[test]
    fn starts_idle_until_spawned() {
        let (handle, runner) = Handle::new(0);
        let heartbeat = handle.heartbeat.clone();
        let worker = std::thread::spawn(move || runner.run());

        let sleeping = wait_until(Duration::from_secs(1), || {
            heartbeat.sleeping.load(Ordering::Relaxed)
        });
        assert!(sleeping);
        assert_eq!(heartbeat.counter.load(Ordering::Relaxed), 0);

        drop(handle);
        worker.join().unwrap();
    }

    #[test]
    fn wakes_for_new_spawn() {
        let (handle, runner) = Handle::new(0);
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || runner.run());

        let sleeping = wait_until(Duration::from_secs(1), || {
            handle.heartbeat.sleeping.load(Ordering::Relaxed)
        });
        assert!(sleeping);

        handle.spawn(async move {
            sender.send(()).expect("channel send failed");
        });

        // The channel receive proves the parked worker woke and ran the spawned task: a sleeping
        // worker cannot poll, so the send only happens after the spawn woke it.
        receiver.recv_timeout(Duration::from_secs(1)).unwrap();

        // Confirm the worker actually left the park and drove a poll cycle. The heartbeat counter
        // is monotonic and only advances inside `poll`, so this is a stable signal — unlike reading
        // the transient `sleeping` flag, which the worker clears and re-sets around the (instant)
        // task and a fast machine can miss.
        let progressed = wait_until(Duration::from_secs(1), || {
            handle.heartbeat.counter.load(Ordering::Relaxed) > 0
        });
        assert!(progressed);
        drop(handle);
        worker.join().unwrap();
    }

    /// The load gauges read the heartbeat the watchdog keeps: a parked worker counts as
    /// `sleeping` and reports no pass rate (zero is suppressed); once it has a task that stays
    /// `Pending` it counts as `spinning` and the pass rate is positive.
    #[test]
    fn load_gauges_follow_the_worker() {
        let inner = s2n_quic_dc_metrics::Registry::new();
        let registry = crate::counter::Registry::with_inner(inner.clone());

        let (handle, runner) = Handle::new(7);
        let pool: Pool = vec![handle.clone()].into();
        let worker = std::thread::spawn(move || runner.run());
        pool.register_load_gauges(&registry, |id| format!("worker.{id}"));

        let sleeping = wait_until(Duration::from_secs(1), || handle.is_sleeping());
        assert!(sleeping);

        // The first sample sets the baseline; it and a parked worker's rate both read 0.
        let line = inner
            .try_take_current_metrics_line_sparse(true)
            .unwrap_or_default();
        assert!(!line.contains("busy_poll.pass_rate"), "got: {line}");
        assert!(
            line.contains("busy_poll.workers=1 Variant|sleeping"),
            "got: {line}"
        );
        assert!(!line.contains("Variant|spinning"), "got: {line}");

        // A task that is never ready keeps the worker in its spin loop.
        handle.spawn(std::future::pending::<()>());
        let spinning = wait_until(Duration::from_secs(1), || handle.passes() > 0);
        assert!(spinning);
        std::thread::sleep(Duration::from_millis(20));

        let line = inner
            .try_take_current_metrics_line_sparse(true)
            .expect("metrics were recorded");
        let rate = line
            .split_whitespace()
            .find_map(|field| field.strip_prefix("busy_poll.pass_rate="))
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or_else(|| panic!("no pass rate on: {line}"));
        assert!(rate > 0, "got: {line}");
        assert!(line.contains("Variant|worker.7"), "got: {line}");
        assert!(
            line.contains("busy_poll.workers=1 Variant|spinning"),
            "got: {line}"
        );
        assert!(!line.contains("Variant|sleeping"), "got: {line}");

        drop((handle, pool));
        worker.join().unwrap();
    }
}
