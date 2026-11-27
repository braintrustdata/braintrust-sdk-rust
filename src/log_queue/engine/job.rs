use super::{intake::Entry, partition::Key, Engine};
use crate::log_queue::report::{Outcome, Report};
use crate::{ExecutionError, WriteError};
use futures::FutureExt;
use std::collections::HashSet;
use std::sync::Arc;

pub(super) struct Job {
    pub(super) engine: Arc<Engine>,
    pub(super) key: Key,
    pub(super) entries: Vec<Option<Entry>>,
    pub(super) report: Report,
    pub(super) retry_cancelled: bool,
    runtime: Option<tokio::runtime::Id>,
    finished: bool,
}

impl Job {
    pub(super) fn new(
        engine: Arc<Engine>,
        key: Key,
        entries: Vec<Entry>,
        runtime: Option<tokio::runtime::Id>,
    ) -> Self {
        Self {
            report: Report::new(entries.len()),
            engine,
            key,
            entries: entries.into_iter().map(Some).collect(),
            retry_cancelled: false,
            runtime,
            finished: false,
        }
    }

    pub(super) async fn run(mut self) {
        let request_scope =
            crate::scheduler::Scope::new(&self.key.route.endpoint, &self.key.route.org);
        let sequences: Vec<_> = self
            .entries
            .iter()
            .flatten()
            .map(|entry| entry.sequence)
            .collect();
        let engine = self.engine.clone();
        let cancelled = async {
            loop {
                let wake = engine.cancellations.notified();
                {
                    let intake = engine.intake.lock().unwrap();
                    if sequences.iter().all(|id| !intake.records.contains_key(id)) {
                        return;
                    }
                }
                wake.await;
            }
        };
        let work = std::panic::AssertUnwindSafe(
            crate::scheduler::SCOPE.scope(request_scope, self.dispatch()),
        )
        .catch_unwind();
        let panicked = tokio::select! {
            biased;
            _ = cancelled => false,
            outcome = work => outcome.is_err(),
        };
        if panicked {
            self.report.settle_delivered();
            let error = crate::log_queue::observation::DeliveryError::new(anyhow::anyhow!(
                "delivery partition panicked"
            ));
            for (index, entry) in self.entries.iter().enumerate() {
                if entry.is_some() && matches!(self.report.outcomes[index], Outcome::Pending) {
                    self.report.failed(vec![index], &error, true);
                }
            }
        }
        self.finished = true;
    }

    pub(super) fn discard_expired(&mut self) {
        let intake = self.engine.intake.lock().unwrap();
        let expired: Vec<_> = self
            .entries
            .iter_mut()
            .filter_map(|entry| {
                if entry
                    .as_ref()
                    .is_some_and(|entry| !intake.records.contains_key(&entry.sequence))
                {
                    entry.take()
                } else {
                    None
                }
            })
            .collect();
        drop(intake);
        self.engine.release_entries(expired);
    }

    pub(super) fn retire_ready(&mut self) {
        {
            let intake = self.engine.intake.lock().unwrap();
            for loss in std::mem::take(&mut self.report.losses) {
                let mut visited = HashSet::new();
                for index in loss.submissions {
                    if let Some(entry) = &self.entries[index] {
                        if intake.records.contains_key(&entry.sequence) {
                            entry
                                .core
                                .observation
                                .record_loss(loss.preparation, &mut visited);
                        }
                    }
                }
            }
        }
        let mut released = Vec::new();
        for index in std::mem::take(&mut self.report.ready) {
            if let Some(entry) = self.entries[index].take() {
                let error = match &self.report.outcomes[index] {
                    Outcome::Delivered => None,
                    Outcome::Failed(error) => Some(error.write_error()),
                    Outcome::Pending => unreachable!("ready write has no outcome"),
                };
                released.push((entry.release(), error));
            }
        }
        self.engine.retire_entries(released);
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.report.settle_delivered();
        self.retire_ready();
        if !self.finished && self.entries.iter().any(Option::is_some) {
            if let Some(runtime) = self.runtime {
                crate::runtime::mark_stopped(runtime);
            }
        }
        let engine = &self.engine;
        let mut intake = engine.intake.lock().unwrap();
        let mut requeue = Vec::new();
        let mut discarded = Vec::new();
        for entry in std::mem::take(&mut self.entries).into_iter().flatten() {
            if (!self.finished || self.retry_cancelled)
                && intake.records.contains_key(&entry.sequence)
            {
                requeue.push(entry);
            } else {
                discarded.push(entry);
            }
        }
        let partition = intake
            .partitions
            .get_mut(&self.key)
            .expect("running partition");
        partition.running = false;
        for entry in requeue.into_iter().rev() {
            partition.pending.push_front(entry);
        }
        let ready = !partition.pending.is_empty();
        if !ready {
            intake.partitions.remove(&self.key);
        }
        drop(intake);
        engine.retire_entries(
            discarded
                .into_iter()
                .map(|entry| {
                    (
                        entry.release(),
                        Some(WriteError::Execution(ExecutionError::WorkerLost)),
                    )
                })
                .collect(),
        );
        if ready {
            engine.schedule(self.key.clone());
        }
    }
}
