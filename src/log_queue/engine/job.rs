use super::{
    intake::{Entry, FinishedWrites},
    partition::Key,
    Engine,
};
use crate::log_queue::report::{Outcome, Report};
use crate::{ExecutionError, WriteError};
use futures::FutureExt;
use std::collections::HashSet;
use std::sync::{Arc, PoisonError};

impl Job {
    pub(super) fn new(engine: Arc<Engine>, key: Key, entries: Vec<Entry>) -> Self {
        Self {
            report: Report::new(entries.len()),
            engine,
            key,
            entries,
        }
    }

    pub(super) async fn run(mut self) {
        let job = &mut self;
        let count = job.entries.len();
        let settings = &job.entries[0].core.settings;
        let endpoint = &job.key.route.endpoint;
        let org_name = job.key.org_name.as_deref();
        let request_scope = crate::scheduler::Scope::new(endpoint, &job.key.route.org);
        let commands: Vec<_> = job.entries.iter().map(|entry| &entry.command).collect();
        let entries = &job.entries;
        let engine = &job.engine;
        let work = std::panic::AssertUnwindSafe(crate::scheduler::SCOPE.scope(
            request_scope,
            settings.dispatch(&commands, &mut job.report, endpoint, org_name),
        ))
        .catch_unwind();
        let cancelled = async {
            loop {
                let wake = engine.cancellations.notified();
                {
                    let intake = engine.intake.lock().unwrap();
                    if entries
                        .iter()
                        .all(|entry| !intake.records.contains_key(&entry.sequence))
                    {
                        return;
                    }
                }
                wake.await;
            }
        };
        let panicked = tokio::select! {
            outcome = work => outcome.is_err(),
            _ = cancelled => false,
        };
        if panicked {
            let error = anyhow::anyhow!("delivery partition panicked");
            for index in 0..count {
                if matches!(job.report.outcomes[index], Outcome::Pending) {
                    job.report.failed(vec![index], &error, true);
                }
            }
        }
        // A completed job has an outcome for every submission.
        job.report.settle(0..count);
    }
}

/// A job retires accepted rows even if its runtime drops the task.
pub(super) struct Job {
    engine: Arc<Engine>,
    key: Key,
    entries: Vec<Entry>,
    report: Report,
}
impl Drop for Job {
    fn drop(&mut self) {
        let entries = std::mem::take(&mut self.entries);
        let report = std::mem::replace(&mut self.report, Report::new(0));
        let engine = &self.engine;
        let mut intake = engine.intake.lock().unwrap_or_else(PoisonError::into_inner);
        for loss in report.losses {
            let mut visited = HashSet::new();
            for index in loss.submissions {
                let entry = &entries[index];
                if intake.records.contains_key(&entry.sequence) {
                    entry
                        .core
                        .observation
                        .record_loss(loss.preparation, &mut visited);
                }
            }
        }
        let mut completed = FinishedWrites::default();
        let mut discarded = Vec::new();
        for (entry, outcome) in entries.into_iter().zip(report.outcomes) {
            let error = match outcome {
                Outcome::Pending => Some(WriteError::Execution(ExecutionError::WorkerLost)),
                Outcome::Delivered => None,
                Outcome::Failed(error) => Some(error.write_error()),
            };
            completed.retire(&mut intake, entry.sequence, error);
            discarded.push(entry);
        }
        let partition = intake
            .partitions
            .get_mut(&self.key)
            .expect("running partition");
        partition.running = false;
        let ready = !partition.pending.is_empty();
        if !ready {
            intake.partitions.remove(&self.key);
        }
        drop(intake);
        drop(discarded);
        completed.notify();
        if ready {
            engine.schedule(self.key.clone());
        }
    }
}
