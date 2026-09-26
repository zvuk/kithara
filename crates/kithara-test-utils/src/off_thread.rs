use kithara_platform::{
    sync::{Mutex, mpsc, mpsc::RecvTimeoutError},
    thread::spawn_named,
    time::{Duration, Instant},
    tokio::{runtime::Handle, sync::oneshot},
};

type Job<T> = Box<dyn FnOnce(&mut T) + Send>;

pub struct OffThread<T> {
    name: &'static str,
    done: Mutex<Option<oneshot::Receiver<()>>>,
    jobs: Mutex<mpsc::Sender<Job<T>>>,
}

impl<T: 'static> OffThread<T> {
    /// Runs `job` against `T` on the owner thread and awaits its result.
    ///
    /// # Panics
    ///
    /// Panics when the owner thread has stopped accepting calls, or dies
    /// before the job returns.
    pub async fn call<R, F>(&self, job: F) -> R
    where
        R: Send + 'static,
        F: FnOnce(&mut T) -> R + Send + 'static,
    {
        let (answer, receiver) = oneshot::channel();
        let request = Box::new(move |value: &mut T| {
            drop(answer.send(job(value)));
        });
        if self.jobs.lock().send(request).is_err() {
            panic!(
                "OffThread owner thread `{}` stopped before accepting a call",
                self.name
            );
        }
        receiver.await.unwrap_or_else(|_| {
            panic!(
                "OffThread owner thread `{}` panicked before returning a call result",
                self.name
            )
        })
    }

    /// Drops the job sender and waits for the owner to drop `T`.
    ///
    /// # Panics
    ///
    /// Panics when the owner thread dies before teardown completes.
    pub async fn close(self) {
        let name = self.name;
        drop(self.jobs);
        let done = self
            .done
            .lock()
            .take()
            .expect("OffThread completion receiver must remain present until close");
        done.await.unwrap_or_else(|_| {
            panic!("OffThread owner thread `{name}` panicked before teardown completed")
        });
    }

    async fn serving<E, I, F>(name: &'static str, init: I, serve: F) -> Result<Self, E>
    where
        E: Send + 'static,
        I: FnOnce() -> Result<T, E> + Send + 'static,
        F: FnOnce(&mpsc::Receiver<Job<T>>, &mut T) + Send + 'static,
    {
        let (jobs, receiver) = mpsc::channel::<Job<T>>();
        let (ready, ready_receiver) = oneshot::channel();
        let (done, done_receiver) = oneshot::channel();
        let runtime = Handle::current();

        drop(spawn_named(name, move || {
            let _runtime = runtime.enter();
            let mut value = match init() {
                Ok(value) => value,
                Err(error) => {
                    drop(ready.send(Err(error)));
                    return;
                }
            };
            if ready.send(Ok(())).is_err() {
                return;
            }
            serve(&receiver, &mut value);
            drop(value);
            let _ = done.send(());
        }));

        match ready_receiver.await.unwrap_or_else(|_| {
            panic!("OffThread owner thread `{name}` panicked during initialization")
        }) {
            Ok(()) => Ok(Self {
                name,
                jobs: Mutex::new(jobs),
                done: Mutex::new(Some(done_receiver)),
            }),
            Err(error) => {
                let _ = done_receiver.await;
                Err(error)
            }
        }
    }

    /// Runs `init` on the owner thread, so `T` does not need to be `Send`.
    ///
    /// The owner thread enters the caller's runtime for its whole life, the
    /// way the product app thread does, so a call may spawn tasks.
    ///
    /// # Errors
    ///
    /// Returns whatever `init` failed with, after the owner thread has exited.
    ///
    /// # Panics
    ///
    /// Panics when the owner thread dies before it reports readiness.
    pub async fn spawn<E, I>(name: &'static str, init: I) -> Result<Self, E>
    where
        E: Send + 'static,
        I: FnOnce() -> Result<T, E> + Send + 'static,
    {
        Self::serving(name, init, |receiver, value| {
            while let Ok(job) = receiver.recv() {
                job(value);
            }
        })
        .await
    }

    /// Like [`Self::spawn`], plus `tick` once per `interval` of the clock —
    /// the audio-device callback a device-free session has no device to
    /// receive. Like a device clock, the schedule does not wait on the owner:
    /// a call or a slow tick delays the ticks due meanwhile, which then run
    /// back to back, so the tick count keeps pace with the clock. `tick` is
    /// serialized with the calls, so it never observes a half-applied one.
    ///
    /// # Errors
    ///
    /// Returns whatever `init` failed with, after the owner thread has exited.
    ///
    /// # Panics
    ///
    /// Panics when the owner thread dies before it reports readiness.
    pub async fn spawn_paced<E, I, P>(
        name: &'static str,
        init: I,
        interval: Duration,
        mut tick: P,
    ) -> Result<Self, E>
    where
        E: Send + 'static,
        I: FnOnce() -> Result<T, E> + Send + 'static,
        P: FnMut(&mut T) + Send + 'static,
    {
        Self::serving(name, init, move |receiver, value| {
            let mut due = Instant::now() + interval;
            loop {
                match receiver.recv_timeout(due) {
                    Ok(job) => job(value),
                    Err(RecvTimeoutError::Timeout) => {
                        tick(value);
                        due += interval;
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        rc::Rc,
        sync::atomic::{AtomicBool, Ordering},
    };

    use kithara_platform::{sync::Arc, thread, time::Duration, tokio::sync::oneshot};
    use kithara_test_utils::kithara;

    use super::OffThread;

    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    #[kithara::test(tokio)]
    async fn call_returns_value_from_owner() {
        let owner = OffThread::spawn("off-thread-call", || Ok::<_, ()>(41_u32))
            .await
            .expect("owner initialization must succeed");

        let value = owner
            .call(|value| {
                *value += 1;
                *value
            })
            .await;

        assert_eq!(value, 42);
        owner.close().await;
    }

    #[kithara::test(tokio)]
    async fn non_send_value_stays_on_owner_thread() {
        let owner = OffThread::spawn("off-thread-non-send", || {
            Ok::<_, ()>(Rc::new(Cell::new(1_u32)))
        })
        .await
        .expect("owner must initialize its non-Send value");

        let value = owner
            .call(|value| {
                value.set(value.get() + 1);
                value.get()
            })
            .await;

        assert_eq!(value, 2);
        owner.close().await;
    }

    #[kithara::test(tokio)]
    async fn init_error_is_returned() {
        let error = OffThread::<()>::spawn("off-thread-init-error", || Err("init failed"))
            .await
            .err()
            .expect("failed initialization must return its error");

        assert_eq!(error, "init failed");
    }

    #[kithara::test(tokio)]
    async fn close_waits_for_value_drop() {
        let dropped = Arc::new(AtomicBool::new(false));
        let owner_flag = Arc::clone(&dropped);
        let owner = OffThread::spawn("off-thread-drop", move || Ok::<_, ()>(DropFlag(owner_flag)))
            .await
            .expect("owner initialization must succeed");

        owner.close().await;

        assert!(dropped.load(Ordering::Acquire));
    }

    /// A paced owner stands in for an audio device, whose clock does not wait
    /// for the callback: a tick that spends half its interval working must
    /// still leave the owner ticking once per interval of the clock.
    #[kithara::test(tokio)]
    async fn paced_ticks_keep_pace_with_the_clock_however_long_a_tick_takes() {
        const INTERVAL: Duration = Duration::from_millis(10);
        const TICKS: u32 = 50;

        let (reached, reached_receiver) = oneshot::channel();
        let mut reached = Some(reached);
        let started = Instant::now();
        let owner = OffThread::spawn_paced(
            "off-thread-paced",
            || Ok::<_, ()>(0_u32),
            INTERVAL,
            move |ticks| {
                thread::sleep(INTERVAL / 2);
                *ticks += 1;
                if *ticks == TICKS
                    && let Some(reached) = reached.take()
                {
                    let _ = reached.send(started.elapsed());
                }
            },
        )
        .await
        .expect("owner initialization must succeed");

        let elapsed = reached_receiver
            .await
            .expect("the owner must reach the tick count");
        owner.close().await;

        // Ticking only after an idle interval would take 1.5x; a quarter of
        // slack still separates that from keeping pace under a loaded host.
        assert!(
            elapsed < INTERVAL * TICKS * 5 / 4,
            "{TICKS} ticks of {INTERVAL:?} took {elapsed:?}: the owner fell behind the clock"
        );
    }

    #[kithara::test]
    fn handle_is_send_and_sync_for_non_send_value() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<OffThread<Rc<Cell<u32>>>>();
    }
}
