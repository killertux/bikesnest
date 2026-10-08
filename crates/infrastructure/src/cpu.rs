//! Bounded admission for CPU work that must run on Tokio's blocking pool.
//! Execution permits move into closures because started blocking tasks outlive
//! cancellation of the async callers awaiting them.

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Semaphore, TryAcquireError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdmissionError {
    QueueFull,
    TimedOut,
    Closed,
    Join,
}

#[derive(Debug, Clone)]
pub(crate) struct CpuAdmission {
    running: Arc<Semaphore>,
    waiting: Arc<Semaphore>,
    timeout: Duration,
}

impl CpuAdmission {
    pub(crate) fn new(concurrency: usize, queue_capacity: usize, timeout: Duration) -> Self {
        Self {
            running: Arc::new(Semaphore::new(concurrency)),
            waiting: Arc::new(Semaphore::new(queue_capacity)),
            timeout,
        }
    }

    #[cfg(test)]
    pub(crate) fn waiting_available(&self) -> usize {
        self.waiting.available_permits()
    }

    pub(crate) async fn run<T, F>(&self, work: F) -> Result<T, AdmissionError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let execution = match self.running.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(TryAcquireError::Closed) => return Err(AdmissionError::Closed),
            Err(TryAcquireError::NoPermits) => {
                let waiting = self.waiting.clone().try_acquire_owned().map_err(|error| {
                    if matches!(error, TryAcquireError::Closed) {
                        AdmissionError::Closed
                    } else {
                        AdmissionError::QueueFull
                    }
                })?;
                let execution =
                    tokio::time::timeout(self.timeout, self.running.clone().acquire_owned())
                        .await
                        .map_err(|_| AdmissionError::TimedOut)?
                        .map_err(|_| AdmissionError::Closed)?;
                drop(waiting);
                execution
            }
        };

        tokio::task::spawn_blocking(move || {
            let _execution = execution;
            work()
        })
        .await
        .map_err(|_| AdmissionError::Join)
    }
}
