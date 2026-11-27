//! Process-wide admission and dispatch for queued writes.
mod intake;
mod job;
mod partition;

use self::{
    intake::{Entry, FinishedWrites, Intake, Record, WriteSender},
    job::Job,
    partition::Key,
};
use super::{core::LogQueueCore, observation::Observation, submission::SubmitCommand};
use crate::{
    Completion, DeliveryReceipt, EnqueueError, ExecutionError, FlushError, FlushReport,
    ServiceConfig, ShutdownError, ShutdownReport, WriteError,
};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

pub(crate) struct Engine {
    config: ServiceConfig,
    intake: Mutex<Intake>,
    cancellations: Notify,
    #[cfg(test)]
    manual: bool,
}
impl Engine {
    pub fn new(config: ServiceConfig) -> Self {
        Self {
            config,
            intake: Mutex::new(Intake::default()),
            cancellations: Notify::new(),
            #[cfg(test)]
            manual: false,
        }
    }
    #[cfg(test)]
    fn manual(config: ServiceConfig) -> Arc<Self> {
        Arc::new(Self {
            manual: true,
            ..Self::new(config)
        })
    }

    /// Start a job for a partition made ready by submission or retirement.
    fn schedule(self: &Arc<Self>, key: Key) {
        #[cfg(test)]
        if self.manual {
            return;
        }
        // Leave the queued rows in place if no runtime can accept them.
        let Ok(runtime) = crate::runtime::handle() else {
            tracing::error!("no runtime can run SDK delivery");
            return;
        };
        if let Some(entries) = self.take_job(&key) {
            runtime.spawn(self.clone().process_job(key, entries));
        }
    }

    fn take_job(&self, key: &Key) -> Option<Vec<Entry>> {
        let mut intake = self.intake.lock().unwrap();
        let partition = intake.partitions.get_mut(key)?;
        if partition.running || partition.pending.is_empty() {
            return None;
        }
        partition.running = true;
        Some(std::mem::take(&mut partition.pending).into())
    }

    pub fn submit(
        self: &Arc<Self>,
        core: Arc<LogQueueCore>,
        command: SubmitCommand,
    ) -> Completion<DeliveryReceipt, WriteError> {
        let (sender, mut completion) = Completion::pair();
        let result = self.enqueue(core, [(command, Some(sender))]).pop().unwrap();
        completion.record_issuance(result.map_err(WriteError::Admission));
        completion
    }
    pub fn submit_many(
        self: &Arc<Self>,
        core: Arc<LogQueueCore>,
        commands: impl IntoIterator<Item = SubmitCommand>,
    ) {
        self.enqueue(core, commands.into_iter().map(|command| (command, None)));
    }
    fn enqueue(
        self: &Arc<Self>,
        core: Arc<LogQueueCore>,
        commands: impl IntoIterator<Item = (SubmitCommand, Option<WriteSender>)>,
    ) -> Vec<Result<(), EnqueueError>> {
        let prepared: Vec<_> = commands
            .into_iter()
            .map(|(command, sender)| {
                let key = Key::new(&core, &command);
                (command, key, sender)
            })
            .collect();
        let mut rejected = Vec::new();
        let mut results = Vec::new();
        let mut ready = Vec::new();
        let mut intake = self.intake.lock().unwrap();
        for (command, key, sender) in prepared {
            let client_rows = core.observation.len();
            let closed = core.observation.closed();
            if closed
                || self
                    .config
                    .max_rows
                    .is_some_and(|max| intake.records.len() >= max)
                || self
                    .config
                    .max_org_rows
                    .is_some_and(|max| intake.org_rows.get(&key.route).copied().unwrap_or(0) >= max)
                || client_rows >= core.settings.config.queue_max_size()
            {
                let error = if closed {
                    EnqueueError::Closed
                } else {
                    EnqueueError::Full
                };
                results.push(Err(error));
                rejected.push((command, sender, error));
                continue;
            }
            intake.sequence += 1;
            let sequence = intake.sequence;
            core.observation.accept(sequence);
            core.retained(client_rows + 1);
            *intake.org_rows.entry(key.route.clone()).or_default() += 1;
            results.push(Ok(()));
            intake.records.insert(
                sequence,
                Record {
                    observer: core.observation.clone(),
                    route: key.route.clone(),
                    sender,
                },
            );
            let partition = intake.partitions.entry(key.clone()).or_default();
            if !partition.running && partition.pending.is_empty() {
                ready.push(key);
            }
            partition.pending.push_back(Entry {
                core: core.clone(),
                command,
                sequence,
            });
        }
        drop(intake);
        for (command, sender, error) in rejected {
            core.rejected(command);
            if let Some(sender) = sender {
                let _ = sender.send(Err(error.into()));
            }
        }
        for key in ready {
            self.schedule(key);
        }
        results
    }
    /// Delivery is continuous, so a barrier only observes work accepted before it.
    pub fn barrier(&self, scope: &Arc<Observation>) -> Completion<FlushReport, FlushError> {
        let intake = self.intake.lock().unwrap();
        scope.barrier(intake.sequence, false)
    }
    pub fn shutdown(
        self: &Arc<Self>,
        scope: &Arc<Observation>,
        timeout: std::time::Duration,
    ) -> Completion<ShutdownReport, ShutdownError> {
        let (sender, completion) = Completion::pair();
        let intake = self.intake.lock().unwrap();
        scope.close();
        let barrier = scope.barrier(intake.sequence, true);
        drop(intake);
        let engine = self.clone();
        let scope = scope.clone();
        crate::service::global()
            .expect("initialized service")
            .spawn_operation(async move {
                let result = match tokio::time::timeout(timeout, barrier).await {
                    Ok(result) => result
                        .map(|report| ShutdownReport {
                            delivered: report.delivered,
                        })
                        .map_err(ShutdownError::from),
                    Err(_) => {
                        engine.expire(&scope);
                        Err(ExecutionError::TimedOut.into())
                    }
                };
                let _ = sender.send(result);
            });
        completion
    }
    fn expire(&self, scope: &Arc<Observation>) {
        let mut intake = self.intake.lock().unwrap();
        scope.time_out();
        let sequences: Vec<_> = intake
            .records
            .iter()
            .filter(|(_, record)| Arc::ptr_eq(&record.observer, scope))
            .map(|(sequence, _)| *sequence)
            .collect();
        let error = WriteError::Execution(ExecutionError::TimedOut);
        let mut completed = FinishedWrites::default();
        for sequence in sequences {
            completed.retire(&mut intake, sequence, Some(error.clone()));
        }
        let Intake {
            records,
            partitions,
            ..
        } = &mut *intake;
        let mut discarded = Vec::new();
        for partition in partitions.values_mut() {
            let old = std::mem::take(&mut partition.pending);
            for entry in old {
                if records.contains_key(&entry.sequence) {
                    partition.pending.push_back(entry);
                } else {
                    discarded.push(entry);
                }
            }
        }
        partitions.retain(|_, partition| partition.running || !partition.pending.is_empty());
        drop(intake);
        completed.notify();
        drop(discarded);
        self.cancellations.notify_waiters();
    }

    #[cfg(test)]
    fn take_jobs(&self) -> Vec<(Key, Vec<Entry>)> {
        let keys: Vec<_> = self
            .intake
            .lock()
            .unwrap()
            .partitions
            .keys()
            .cloned()
            .collect();
        keys.into_iter()
            .filter_map(|key| self.take_job(&key).map(|entries| (key, entries)))
            .collect()
    }

    /// The job exists before the future is first polled, so a runtime that
    /// drops the task unpolled still reports the lost worker.
    fn process_job(
        self: Arc<Self>,
        key: Key,
        entries: Vec<Entry>,
    ) -> impl std::future::Future<Output = ()> + Send + 'static {
        Job::new(self, key, entries).run()
    }
}

#[cfg(test)]
mod tests;
