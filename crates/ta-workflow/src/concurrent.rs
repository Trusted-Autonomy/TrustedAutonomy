// concurrent.rs — OS-thread task fan-out for wave execution (v0.17.0.12.34).
//
// `execute_swarm` previously ran sub-goals in a strictly serial `for` loop
// (see `apps/ta-cli/src/commands/run.rs`), even though `swarm.rs`'s own doc
// comment always said parallel execution via OS threads was the plan. This
// module is the small, generic primitive that backs real concurrency: run a
// batch of index-tagged closures on their own threads and collect results
// back in the original index order, so a caller can update per-item state
// deterministically after the whole wave completes.

use std::thread;

/// Run each `(index, task)` pair on its own OS thread, then join all of them
/// and return `(index, result)` pairs. Panics inside a task propagate as a
/// panic here (via `JoinHandle::join`'s `Result::unwrap`) — tasks are
/// expected to encode their own failures as `T`, not panic.
pub fn run_concurrently<T: Send + 'static>(
    tasks: Vec<(usize, Box<dyn FnOnce() -> T + Send>)>,
) -> Vec<(usize, T)> {
    let handles: Vec<(usize, thread::JoinHandle<T>)> = tasks
        .into_iter()
        .map(|(index, f)| (index, thread::spawn(f)))
        .collect();

    handles
        .into_iter()
        .map(|(index, handle)| {
            let result = handle
                .join()
                .unwrap_or_else(|e| std::panic::resume_unwind(e));
            (index, result)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    #[test]
    fn results_come_back_tagged_with_original_index() {
        let tasks: Vec<(usize, Box<dyn FnOnce() -> i32 + Send>)> = vec![
            (0, Box::new(|| 10)),
            (1, Box::new(|| 20)),
            (2, Box::new(|| 30)),
        ];
        let mut results = run_concurrently(tasks);
        results.sort_by_key(|(i, _)| *i);
        assert_eq!(results, vec![(0, 10), (1, 20), (2, 30)]);
    }

    #[test]
    fn tasks_genuinely_overlap_in_wall_clock_time() {
        // Three tasks that each sleep 150ms. If they were run serially the
        // total would be >= 450ms; run concurrently it should stay well
        // under that, and each task's start time should land within a
        // small window of the others (not staggered by ~150ms per task).
        let starts: Arc<Mutex<Vec<Instant>>> = Arc::new(Mutex::new(Vec::new()));
        let wall_start = Instant::now();

        let tasks: Vec<(usize, Box<dyn FnOnce() + Send>)> = (0..3)
            .map(|i| {
                let starts = Arc::clone(&starts);
                let task: Box<dyn FnOnce() + Send> = Box::new(move || {
                    starts.lock().unwrap().push(Instant::now());
                    thread::sleep(Duration::from_millis(150));
                });
                (i, task)
            })
            .collect();

        run_concurrently(tasks);
        let elapsed = wall_start.elapsed();

        assert!(
            elapsed < Duration::from_millis(400),
            "expected concurrent wall time well under the serial sum (450ms), got {:?}",
            elapsed
        );

        let starts = starts.lock().unwrap();
        assert_eq!(starts.len(), 3);
        let min = starts.iter().min().unwrap();
        let max = starts.iter().max().unwrap();
        assert!(
            max.duration_since(*min) < Duration::from_millis(100),
            "expected all task start times to overlap within 100ms, spread was {:?}",
            max.duration_since(*min)
        );
    }
}
