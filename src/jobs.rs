//! Background work for a UI that must never block.
//!
//! A [`Job`] runs one future on the tokio runtime and hands its result back
//! through a oneshot channel that the UI polls once per frame. Dropping the
//! job aborts the future, which drops any child process it owns (children
//! are spawned with `kill_on_drop`), so closing a panel cancels its work.

use std::future::Future;
use std::sync::Arc;

use tokio::runtime::Handle;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::error::{Error, Result};

/// Called when a job finishes so the UI can redraw.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone)]
pub struct Spawner {
    handle: Handle,
    wake: Waker,
}

impl Spawner {
    pub fn new(handle: Handle, wake: Waker) -> Self {
        Self { handle, wake }
    }

    pub fn handle(&self) -> &Handle {
        &self.handle
    }

    pub fn wake(&self) {
        (self.wake)();
    }

    /// Runs `fut` in the background.
    pub fn spawn<T, F>(&self, fut: F) -> Job<T>
    where
        T: Send + 'static,
        F: Future<Output = Result<T>> + Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        let wake = self.wake.clone();
        let task = self.handle.spawn(async move {
            let result = fut.await;
            let _ = tx.send(result);
            wake();
        });
        Job { rx: Some(rx), task }
    }
}

pub struct Job<T> {
    rx: Option<oneshot::Receiver<Result<T>>>,
    task: JoinHandle<()>,
}

impl<T> Job<T> {
    /// The result once, as soon as the job has finished.
    pub fn poll(&mut self) -> Option<Result<T>> {
        let rx = self.rx.as_mut()?;
        match rx.try_recv() {
            Ok(result) => {
                self.rx = None;
                Some(result)
            }
            Err(oneshot::error::TryRecvError::Empty) => None,
            Err(oneshot::error::TryRecvError::Closed) => {
                self.rx = None;
                Some(Err(Error::Cancelled))
            }
        }
    }

    pub fn cancel(&self) {
        self.task.abort();
    }
}

impl<T> Drop for Job<T> {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A job slot a panel keeps: at most one operation at a time, with its
/// last result delivered exactly once.
pub struct Slot<T> {
    job: Option<Job<T>>,
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Self { job: None }
    }
}

impl<T> Slot<T> {
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    pub fn set(&mut self, job: Job<T>) {
        self.job = Some(job);
    }

    pub fn cancel(&mut self) {
        self.job = None;
    }

    pub fn poll(&mut self) -> Option<Result<T>> {
        let result = self.job.as_mut()?.poll()?;
        self.job = None;
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    fn spawner(counter: Arc<AtomicUsize>) -> Spawner {
        Spawner::new(
            Handle::current(),
            Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }),
        )
    }

    #[tokio::test]
    async fn delivers_result_once_and_wakes_the_ui() {
        let woken = Arc::new(AtomicUsize::new(0));
        let sp = spawner(woken.clone());
        let mut job = sp.spawn(async { Ok(7) });
        for _ in 0..100 {
            if let Some(r) = job.poll() {
                assert_eq!(r.unwrap(), 7);
                assert!(job.poll().is_none());
                assert_eq!(woken.load(Ordering::SeqCst), 1);
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("job never finished");
    }

    #[tokio::test]
    async fn dropping_a_job_cancels_its_future() {
        struct Flag(Arc<AtomicBool>);
        impl Drop for Flag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let sp = spawner(Arc::new(AtomicUsize::new(0)));
        let flag = Flag(dropped.clone());
        let job = sp.spawn(async move {
            let _flag = flag;
            tokio::time::sleep(Duration::from_secs(60)).await;
            Ok(())
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(job);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(dropped.load(Ordering::SeqCst), "future state must be dropped");
    }

    #[tokio::test]
    async fn slot_reports_busy_until_polled() {
        let sp = spawner(Arc::new(AtomicUsize::new(0)));
        let mut slot: Slot<u8> = Slot::default();
        assert!(!slot.busy());
        slot.set(sp.spawn(async { Err(Error::Invalid("nope".into())) }));
        assert!(slot.busy());
        let mut got = None;
        for _ in 0..100 {
            if let Some(r) = slot.poll() {
                got = Some(r);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(matches!(got, Some(Err(Error::Invalid(_)))));
        assert!(!slot.busy());
    }
}
