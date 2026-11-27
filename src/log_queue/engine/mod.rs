//! Process-wide admission and dispatch for queued writes.
mod dispatch;
mod intake;
mod job;
mod partition;
mod row_order;

use self::{
    intake::{Entry, FinishedWrites, Intake, Record, RowOrderKey, WriteSender},
    job::Job,
    partition::Key,
};
use super::{core::LogQueueCore, observation::Observation, submission::SubmitCommand};
use crate::{
    Completion, DeliveryReceipt, EnqueueError, ExecutionError, FlushError, FlushReport,
    ServiceConfig, ShutdownError, ShutdownReport, WriteError,
};
use futures::FutureExt;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

pub(crate) struct PendingCapacity {
    engine: Arc<Engine>,
    core: Arc<LogQueueCore>,
    root: usize,
    route: Option<partition::OrgKey>,
    count: usize,
}

impl PendingCapacity {
    pub fn grow(&mut self) -> Result<(), EnqueueError> {
        let core = &self.core;
        let mut intake = self.engine.intake.lock().unwrap();
        let client_rows = core.observation.root().len()
            + intake
                .reserved_clients
                .get(&self.root)
                .copied()
                .unwrap_or(0);
        let org_rows = match &self.route {
            Some(route) => {
                intake.org_rows.get(route).copied().unwrap_or(0)
                    + intake.reserved_orgs.get(route).copied().unwrap_or(0)
            }
            None => intake.reserved_unknown,
        };
        if core.observation.closed() {
            return Err(EnqueueError::Closed);
        }
        if self
            .engine
            .config
            .max_rows
            .is_some_and(|max| intake.retained + intake.reserved >= max)
            || self
                .engine
                .config
                .max_org_rows
                .is_some_and(|max| org_rows >= max)
            || client_rows >= core.settings.config.queue_max_size()
        {
            return Err(EnqueueError::Full);
        }
        intake.reserved += 1;
        *intake.reserved_clients.entry(self.root).or_default() += 1;
        if let Some(route) = &self.route {
            *intake.reserved_orgs.entry(route.clone()).or_default() += 1;
        } else {
            intake.reserved_unknown += 1;
        }
        self.count += 1;
        Ok(())
    }
    fn release_one(&mut self, intake: &mut Intake) {
        if self.count == 0 {
            return;
        }
        self.count -= 1;
        intake.reserved -= 1;
        let n = intake.reserved_clients.get_mut(&self.root).unwrap();
        *n -= 1;
        if *n == 0 {
            intake.reserved_clients.remove(&self.root);
        }
        if let Some(route) = &self.route {
            let n = intake.reserved_orgs.get_mut(route).unwrap();
            *n -= 1;
            if *n == 0 {
                intake.reserved_orgs.remove(route);
            }
        } else {
            intake.reserved_unknown -= 1;
        }
    }
}
impl Drop for PendingCapacity {
    fn drop(&mut self) {
        let engine = self.engine.clone();
        let mut intake = engine.intake.lock().unwrap();
        while self.count > 0 {
            self.release_one(&mut intake);
        }
    }
}

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

    pub fn pending_capacity(
        self: &Arc<Self>,
        core: &Arc<LogQueueCore>,
        org_id: Option<&str>,
    ) -> PendingCapacity {
        let route = org_id.filter(|id| !id.is_empty()).map(|id| {
            let (endpoint, name) = core.route(id, None);
            let org = crate::resolution::OrgIdentity::new(id, name.as_deref(), "")
                .request_scope_key(&endpoint);
            partition::OrgKey { endpoint, org }
        });
        PendingCapacity {
            engine: self.clone(),
            core: core.clone(),
            root: core.observation.root() as *const _ as usize,
            route,
            count: 0,
        }
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
            runtime.spawn(self.clone().process_job(key, entries, Some(runtime.id())));
        }
    }

    fn take_job(&self, key: &Key) -> Option<Vec<Entry>> {
        let mut intake = self.intake.lock().unwrap();
        let Intake {
            partitions,
            row_order,
            ..
        } = &mut *intake;
        let partition = partitions.get_mut(key)?;
        if partition.running {
            return None;
        }
        let mut entries = Vec::new();
        let mut selected = HashSet::new();
        while let Some(entry) = partition.pending.front() {
            let predecessor = entry.row.as_ref().and_then(|row| {
                row_order
                    .get(row)?
                    .writes
                    .range(..entry.sequence)
                    .rev()
                    .find(|(_, earlier)| key.aliases(earlier))
            });
            if predecessor
                .is_some_and(|(sequence, _)| !key.merges_spans() || !selected.contains(sequence))
            {
                break;
            }
            selected.insert(entry.sequence);
            entries.push(partition.pending.pop_front().expect("front exists"));
        }
        if entries.is_empty() {
            return None;
        }
        partition.running = true;
        Some(entries)
    }

    pub fn submit(
        self: &Arc<Self>,
        core: Arc<LogQueueCore>,
        command: SubmitCommand,
    ) -> Completion<DeliveryReceipt, WriteError> {
        let (sender, mut completion) = Completion::pair();
        let result = self
            .enqueue(core, [(command, Some(sender))], None)
            .pop()
            .unwrap();
        completion.record_issuance(result.map_err(WriteError::Admission));
        completion
    }
    pub fn submit_reserved(
        self: &Arc<Self>,
        core: Arc<LogQueueCore>,
        command: SubmitCommand,
        permit: &mut PendingCapacity,
    ) -> Completion<DeliveryReceipt, WriteError> {
        let (sender, mut completion) = Completion::pair();
        let result = self
            .enqueue(core, [(command, Some(sender))], Some(permit))
            .pop()
            .unwrap();
        completion.record_issuance(result.map_err(WriteError::Admission));
        completion
    }
    pub fn submit_many(
        self: &Arc<Self>,
        core: Arc<LogQueueCore>,
        commands: impl IntoIterator<Item = SubmitCommand>,
    ) {
        self.enqueue(
            core,
            commands.into_iter().map(|command| (command, None)),
            None,
        );
    }
    fn enqueue(
        self: &Arc<Self>,
        core: Arc<LogQueueCore>,
        commands: impl IntoIterator<Item = (SubmitCommand, Option<WriteSender>)>,
        mut reserved: Option<&mut PendingCapacity>,
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
            if let Some(permit) = reserved.as_deref_mut() {
                permit.release_one(&mut intake);
            }
            let client_rows = core.observation.root().len()
                + intake
                    .reserved_clients
                    .get(&(core.observation.root() as *const _ as usize))
                    .copied()
                    .unwrap_or(0);
            let closed = core.observation.closed();
            if closed
                || self
                    .config
                    .max_rows
                    .is_some_and(|max| intake.retained + intake.reserved >= max)
                || self.config.max_org_rows.is_some_and(|max| {
                    intake.org_rows.get(&key.route).copied().unwrap_or(0)
                        + intake.reserved_orgs.get(&key.route).copied().unwrap_or(0)
                        >= max
                })
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
            intake.retained += 1;
            *intake.org_rows.entry(key.route.clone()).or_default() += 1;
            results.push(Ok(()));
            intake.records.insert(
                sequence,
                Record {
                    observer: core.observation.clone(),
                    sender,
                },
            );
            let row = command.row_id().map(|id| RowOrderKey {
                route: key.route.clone(),
                id: id.to_owned(),
            });
            if let Some(row) = &row {
                intake
                    .row_order
                    .entry(row.clone())
                    .or_default()
                    .insert(sequence, key.clone());
            }

            let route = key.route.clone();
            let partition = intake.partitions.entry(key.clone()).or_default();
            if !partition.running && partition.pending.is_empty() {
                ready.push(key);
            }
            partition.pending.push_back(Entry {
                core: core.clone(),
                command,
                sequence,
                row,
                route,
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
        let (sender, mut completion) = Completion::pair();
        let intake = self.intake.lock().unwrap();
        scope.close();
        let mut barrier = scope.barrier(intake.sequence, true);
        drop(intake);
        if let Err(error) = barrier.submission() {
            completion.record_issuance(Err(error.clone().into()));
            return completion;
        }
        let shutdown_report = |report: FlushReport| ShutdownReport {
            delivered: report.delivered,
        };
        if let Some(result) = (&mut barrier).now_or_never() {
            let _ = sender.send(result.map(shutdown_report).map_err(ShutdownError::from));
            return completion;
        }
        let engine = self.clone();
        let scope = scope.clone();
        crate::service::global()
            .expect("initialized service")
            .spawn_operation(async move {
                let result = match tokio::time::timeout(timeout, barrier).await {
                    Ok(result) => result.map(shutdown_report).map_err(ShutdownError::from),
                    Err(_) => {
                        engine.expire(&scope);
                        Err(ExecutionError::TimedOut.into())
                    }
                };
                let _ = sender.send(result);
            });
        completion
    }
    fn expire(self: &Arc<Self>, scope: &Arc<Observation>) {
        let mut intake = self.intake.lock().unwrap();
        scope.time_out();
        let sequences: Vec<_> = intake
            .records
            .iter()
            .filter(|(_, record)| record.observer.includes(scope))
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
        self.release_entries(discarded);
        self.cancellations.notify_waiters();
    }

    fn release_entries(self: &Arc<Self>, entries: Vec<Entry>) {
        let rows: Vec<_> = entries.into_iter().map(Entry::release).collect();
        let mut intake = self.intake.lock().unwrap();
        let ready = rows
            .into_iter()
            .flat_map(|row| intake.release_row(row))
            .collect::<HashSet<_>>();
        drop(intake);
        for key in ready {
            self.schedule(key);
        }
    }

    fn retire_entries(self: &Arc<Self>, writes: Vec<(intake::ReleasedEntry, Option<WriteError>)>) {
        let mut intake = self.intake.lock().unwrap();
        let mut completed = FinishedWrites::default();
        let mut ready = HashSet::new();
        for (released, error) in writes {
            completed.retire(&mut intake, released.sequence, error);
            ready.extend(intake.release_row(released));
        }
        drop(intake);
        completed.notify();
        for key in ready {
            self.schedule(key);
        }
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
    /// drops the task unpolled still requeues its rows.
    fn process_job(
        self: Arc<Self>,
        key: Key,
        entries: Vec<Entry>,
        runtime: Option<tokio::runtime::Id>,
    ) -> impl std::future::Future<Output = ()> + Send + 'static {
        Job::new(self, key, entries, runtime).run()
    }
}

#[cfg(test)]
mod tests;
