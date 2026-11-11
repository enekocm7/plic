//! A small pool of threads for blocking jobs such as running ffprobe.
//!
//! Each job usually spawns an ffmpeg process, so the pool bounds how many run
//! at once; GPUI's background executor is left free for short work.

use std::{
    sync::{Arc, Mutex, mpsc},
    thread,
};

use futures::channel::oneshot;

type Job = Box<dyn FnOnce() + Send>;

pub struct WorkerPool {
    jobs: mpsc::Sender<Job>,
}

impl WorkerPool {
    pub fn new(name: &str, threads: usize) -> Self {
        let (jobs, queue) = mpsc::channel::<Job>();
        let queue = Arc::new(Mutex::new(queue));
        for index in 0..threads.max(1) {
            let queue = Arc::clone(&queue);
            thread::Builder::new()
                .name(format!("{name}-{index}"))
                .spawn(move || {
                    loop {
                        // Hold the lock only while taking a job, not while running it.
                        let job = queue.lock().map(|queue| queue.recv());
                        match job {
                            Ok(Ok(job)) => job(),
                            _ => return,
                        }
                    }
                })
                .expect("failed to start a worker thread");
        }
        Self { jobs }
    }

    /// Queues `job` and returns a receiver for its result. The receiver
    /// resolves to `Err(Canceled)` only if the pool shuts down first.
    pub fn run<T, F>(&self, job: F) -> oneshot::Receiver<T>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let (sender, receiver) = oneshot::channel();
        let _ = self.jobs.send(Box::new(move || {
            // The caller may have stopped waiting; that is not an error.
            let _ = sender.send(job());
        }));
        receiver
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_jobs_and_returns_results() {
        let pool = WorkerPool::new("test", 2);
        let receivers: Vec<_> = (0..8).map(|value| pool.run(move || value * 2)).collect();
        let results: Vec<i32> = receivers
            .into_iter()
            .map(|receiver| futures::executor::block_on(receiver).unwrap())
            .collect();
        assert_eq!(results, [0, 2, 4, 6, 8, 10, 12, 14]);
    }
}
