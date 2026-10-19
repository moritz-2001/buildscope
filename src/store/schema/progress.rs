//! Terminal feedback for one-time upgrades. A separate reporter keeps long SQLite
//! scans and journal flushes visible without sharing the database connection.
use std::{
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// Owns the upgrade reporter; dropping it stops and joins the background thread.
pub(super) struct MigrationProgress {
    /// Current stage and processed count, shared only with the reporter thread.
    state: Arc<Mutex<Stage>>,
    /// Wakes the reporter immediately when the upgrade finishes or fails.
    stop: mpsc::Sender<()>,
    /// Joined on drop so no progress message can outlive its upgrade.
    reporter: Option<JoinHandle<()>>,
    /// Start of this opener's wait, including time spent waiting for another writer.
    started: Instant,
}

struct Stage {
    label: &'static str,
    rows: u64,
    started: Instant,
}

impl MigrationProgress {
    pub(super) fn new() -> Self {
        let state = Arc::new(Mutex::new(Stage {
            label: "Waiting for database write access",
            rows: 0,
            started: Instant::now(),
        }));
        eprintln!(
            "Buildscope migration: upgrading the entire database to version 2; original JSON is retained."
        );
        eprintln!("Buildscope migration: waiting for database write access...");
        let (stop, receiver) = mpsc::channel();
        let reporter_state = Arc::clone(&state);
        let reporter = thread::spawn(move || {
            while let Err(mpsc::RecvTimeoutError::Timeout) =
                receiver.recv_timeout(Duration::from_secs(2))
            {
                let stage = reporter_state.lock().unwrap();
                log_stage(&stage);
            }
        });
        Self {
            state,
            stop,
            reporter: Some(reporter),
            started: Instant::now(),
        }
    }

    pub(super) fn stage(&self, label: &'static str) {
        let mut stage = self.state.lock().unwrap();
        if stage.rows > 0 {
            log_stage(&stage);
        }
        *stage = Stage {
            label,
            rows: 0,
            started: Instant::now(),
        };
        eprintln!("Buildscope migration: {label}...");
    }

    pub(super) fn rows(&self, processed: u64) {
        self.state.lock().unwrap().rows = processed;
    }

    pub(super) fn finish(mut self) {
        // Stop the heartbeat before announcing completion, including journal flushing.
        self.stop_reporter();
        eprintln!(
            "Buildscope migration: complete in {:.1}s; original JSON retained.",
            self.started.elapsed().as_secs_f64()
        );
    }

    fn stop_reporter(&mut self) {
        let _ = self.stop.send(());
        if let Some(reporter) = self.reporter.take() {
            let _ = reporter.join();
        }
    }
}

impl Drop for MigrationProgress {
    fn drop(&mut self) {
        self.stop_reporter();
    }
}

fn log_stage(stage: &Stage) {
    eprintln!(
        "Buildscope migration: {}{} ({:.1}s elapsed)",
        stage.label,
        if stage.rows > 0 {
            format!(": {} rows processed", stage.rows)
        } else {
            String::new()
        },
        stage.started.elapsed().as_secs_f64()
    );
}
