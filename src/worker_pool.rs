//! Bounded parallel preparation with ordered, serial consumption. Each worker owns
//! one outstanding item, so a slow early item cannot create an unbounded result queue.
use anyhow::{Context, Result, ensure};
use std::{sync::mpsc, thread};

/// Prepare items concurrently, then consume them in slice order on the calling thread.
/// At most `workers` items are in preparation or awaiting consumption. Every worker
/// is joined on success, preparation/consumer errors, or a worker panic.
pub(crate) fn process_ordered<T: Sync, R: Send>(
    items: &[T],
    workers: usize,
    prepare: impl Fn(&T) -> Result<R> + Sync,
    mut consume: impl FnMut(R) -> Result<()>,
) -> Result<()> {
    ensure!(workers > 0, "worker count must be positive");
    if items.is_empty() {
        return Ok(());
    }
    let count = workers.min(items.len());
    thread::scope(|scope| {
        let mut channels = Vec::with_capacity(count);
        let mut handles = Vec::with_capacity(count);
        for _ in 0..count {
            let (jobs, incoming) = mpsc::sync_channel::<usize>(1);
            let (completed, results) = mpsc::sync_channel(1);
            let prepare = &prepare;
            handles.push(scope.spawn(move || {
                for index in incoming {
                    if completed.send(prepare(&items[index])).is_err() {
                        break;
                    }
                }
            }));
            channels.push((jobs, results));
        }

        let result = (|| -> Result<()> {
            for (index, (jobs, _)) in channels.iter().enumerate() {
                jobs.send(index)
                    .context("decoder worker stopped before accepting work")?;
            }
            for index in 0..items.len() {
                let (jobs, results) = &channels[index % count];
                let prepared = results
                    .recv()
                    .context("decoder worker stopped before returning a result")??;
                consume(prepared)?;
                // Reuse this worker's slot only after its previous result was consumed.
                if index + count < items.len() {
                    jobs.send(index + count)
                        .context("decoder worker stopped before accepting work")?;
                }
            }
            Ok(())
        })();

        // Disconnect both directions before joining: failed consumers must release
        // queued results and wake workers waiting for another job or a result reader.
        drop(channels);
        let mut panicked = false;
        for handle in handles {
            panicked |= handle.join().is_err();
        }
        result?;
        ensure!(!panicked, "decoder worker panicked");
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc, Barrier, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn concurrent_results_are_bounded_and_consumed_in_input_order() {
        let workers = 3;
        let inputs: Vec<_> = (0..18).collect();
        let started = Barrier::new(workers);
        let completed = (Mutex::new(0usize), Condvar::new());
        let live = Arc::new(AtomicUsize::new(0));
        let maximum = AtomicUsize::new(0);
        let mut received = Vec::new();
        process_ordered(
            &inputs,
            workers,
            |&index| {
                let count = live.fetch_add(1, Ordering::SeqCst) + 1;
                maximum.fetch_max(count, Ordering::SeqCst);
                let value = Tracked {
                    index,
                    live: live.clone(),
                };
                if index < workers {
                    started.wait();
                    let (count, ready) = &completed;
                    let mut count = count.lock().unwrap();
                    if index == 0 {
                        // Force later results to finish while the writer waits for the first.
                        while *count < workers - 1 {
                            count = ready.wait(count).unwrap();
                        }
                    } else {
                        *count += 1;
                        ready.notify_all();
                    }
                }
                Ok(value)
            },
            |value| {
                received.push(value.index);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(received, inputs);
        assert_eq!(maximum.load(Ordering::SeqCst), workers);
        assert_eq!(live.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn consumer_failure_drops_pending_results_and_joins_workers() {
        let live = Arc::new(AtomicUsize::new(0));
        let prepared = AtomicUsize::new(0);
        let result = process_ordered(
            &(0..100).collect::<Vec<_>>(),
            4,
            |&index| {
                prepared.fetch_add(1, Ordering::SeqCst);
                live.fetch_add(1, Ordering::SeqCst);
                Ok(Tracked {
                    index,
                    live: live.clone(),
                })
            },
            |_| anyhow::bail!("writer failed"),
        );
        assert_eq!(result.unwrap_err().to_string(), "writer failed");
        assert!(prepared.load(Ordering::SeqCst) <= 4);
        assert_eq!(live.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn preparation_errors_and_panics_stop_at_their_input_position() {
        for panic in [false, true] {
            let mut received = Vec::new();
            let result = process_ordered(
                &[0, 1, 2, 3, 4],
                3,
                |&index| {
                    if index == 1 {
                        assert!(!panic, "worker panic");
                        anyhow::bail!("preparation failed");
                    }
                    Ok(index)
                },
                |value| {
                    received.push(value);
                    Ok(())
                },
            );
            assert!(result.is_err());
            assert_eq!(received, [0]);
        }
    }

    #[test]
    fn empty_and_single_worker_batches_preserve_the_same_contract() {
        let mut received = Vec::new();
        process_ordered::<usize, usize>(&[], 4, |&value| Ok(value), |_| unreachable!()).unwrap();
        process_ordered(
            &[3, 2, 1],
            1,
            |&value| Ok(value),
            |value| {
                received.push(value);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(received, [3, 2, 1]);
    }

    struct Tracked {
        index: usize,
        live: Arc<AtomicUsize>,
    }

    impl Drop for Tracked {
        fn drop(&mut self) {
            self.live.fetch_sub(1, Ordering::SeqCst);
        }
    }
}
