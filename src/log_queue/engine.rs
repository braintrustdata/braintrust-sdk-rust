//! One intake and dispatcher for every facade. Observation and routing are independent.
use super::dispatch::Report;
use super::{
    observation::Observation,
    queue::{Credential, LogQueueCore, SubmitCommand},
};
use crate::ServiceConfig;
use crate::{
    Completion, DeliveryReceipt, EnqueueError, ExecutionError, FlushError, FlushReport,
    ShutdownError, ShutdownReport, WriteError,
};
use futures::FutureExt;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};
use tokio::sync::{oneshot, Notify};

/// An org at one endpoint: what budgets and request scheduling count against.
#[derive(Hash, PartialEq, Eq, Clone)]
struct OrgKey {
    endpoint: String,
    /// See [`crate::resolution::OrgIdentity::scope_key`].
    org: String,
}

/// One delivery partition: an org's rows that can share requests. Each has at
/// most one job in flight, so an org that is retrying or cooling down never
/// delays another org's uploads.
#[derive(Hash, PartialEq, Eq, Clone)]
struct Key {
    route: OrgKey,
    /// The organization name sent with requests.
    org_name: Option<String>,
    credential: Credential,
    policy: Arc<super::config::LogQueueConfig>,
    transport: u64,
    app_url: String,
    raw: bool,
}
struct Entry {
    core: Arc<LogQueueCore>,
    command: SubmitCommand,
    sequence: u64,
}
type WriteSender = oneshot::Sender<Result<DeliveryReceipt, WriteError>>;
struct Record {
    observer: Arc<Observation>,
    bytes: usize,
    route: OrgKey,
    sender: Option<WriteSender>,
}
#[derive(Default)]
struct Intake {
    sequence: u64,
    records: HashMap<u64, Record>,
    bytes: usize,
    partitions: HashMap<Key, VecDeque<Entry>>,
    active: HashSet<Key>,
    org_rows: HashMap<OrgKey, usize>,
}

pub(crate) struct Engine {
    config: ServiceConfig,
    intake: Mutex<Intake>,
    cancellations: Notify,
    /// Set by the service, which runs jobs; standalone engines in tests start none.
    this: OnceLock<Weak<Engine>>,
}
impl Engine {
    pub fn new(config: ServiceConfig) -> Self {
        Self {
            config,
            intake: Mutex::new(Intake::default()),
            cancellations: Notify::new(),
            this: OnceLock::new(),
        }
    }
    /// Start jobs from now on, as rows arrive and earlier jobs finish.
    pub fn start_jobs(self: &Arc<Self>) {
        let _ = self.this.set(Arc::downgrade(self));
    }
    /// Start a job for a partition made ready by submission or retirement.
    fn schedule(&self, key: Key) {
        let Some((engine, service)) = self
            .this
            .get()
            .and_then(Weak::upgrade)
            .zip(crate::service::started())
        else {
            return;
        };
        // Leave the queued rows in place if no runtime can accept them.
        let Some(runtime) = service.runtime_handle() else {
            tracing::error!("no runtime can run SDK delivery");
            return;
        };
        if let Some(entries) = self.take_job(&key) {
            runtime.spawn(engine.process_job(key, entries, Some(runtime.id())));
        }
    }

    fn take_job(&self, key: &Key) -> Option<Vec<Entry>> {
        let mut intake = self.intake.lock().unwrap();
        if intake.active.contains(key) {
            return None;
        }
        let entries = intake.partitions.remove(key)?;
        intake.active.insert(key.clone());
        Some(entries.into())
    }

    pub fn submit(
        &self,
        core: Arc<LogQueueCore>,
        command: SubmitCommand,
    ) -> Completion<DeliveryReceipt, WriteError> {
        let (sender, mut completion) = Completion::pair();
        let result = self.enqueue(core, [(command, Some(sender))]).pop().unwrap();
        completion.record_issuance(result.map_err(WriteError::Admission));
        completion
    }
    pub fn submit_many(
        &self,
        core: Arc<LogQueueCore>,
        commands: impl IntoIterator<Item = SubmitCommand>,
    ) {
        self.enqueue(core, commands.into_iter().map(|command| (command, None)));
    }
    fn enqueue(
        &self,
        core: Arc<LogQueueCore>,
        commands: impl IntoIterator<Item = (SubmitCommand, Option<WriteSender>)>,
    ) -> Vec<Result<(), EnqueueError>> {
        let prepared: Vec<_> = commands
            .into_iter()
            .map(|(command, sender)| {
                let bytes = self
                    .config
                    .max_bytes
                    .map_or(0, |_| command.estimated_bytes());
                let partition = command.partition();
                (command, bytes, partition, sender)
            })
            .collect();
        let mut rejected = Vec::new();
        let mut results = Vec::new();
        let mut ready = Vec::new();
        let mut intake = self.intake.lock().unwrap();
        for (command, bytes, (credential, org, org_name), sender) in prepared {
            let (endpoint, org_name) = core.route(&org, org_name.as_deref());
            let route = OrgKey {
                endpoint,
                org: crate::resolution::OrgIdentity::new(
                    &org,
                    org_name.as_deref(),
                    &credential.stand_in(),
                )
                .scope_key(),
            };

            let mut root = &core.observation;
            while let Some(parent) = &root.parent {
                root = parent;
            }
            let client_rows = root.len();
            let closed = core.observation.closed();
            if closed
                || self
                    .config
                    .max_rows
                    .is_some_and(|max| intake.records.len() >= max)
                || self
                    .config
                    .max_bytes
                    .is_some_and(|max| intake.bytes.saturating_add(bytes) > max)
                || self
                    .config
                    .max_org_rows
                    .is_some_and(|max| intake.org_rows.get(&route).copied().unwrap_or(0) >= max)
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
            *intake.org_rows.entry(route.clone()).or_default() += 1;
            let key = Key {
                route,
                org_name,
                credential,
                policy: core.settings.config.clone(),
                transport: core.settings.client.identity,
                app_url: core.settings.app_url.to_string(),
                raw: command.is_raw(),
            };
            results.push(Ok(()));
            intake.records.insert(
                sequence,
                Record {
                    observer: core.observation.clone(),
                    bytes,
                    route: key.route.clone(),
                    sender,
                },
            );
            let idle = !intake.active.contains(&key);
            let partition = intake.partitions.entry(key.clone()).or_default();
            if idle && partition.is_empty() {
                ready.push(key);
            }
            partition.push_back(Entry {
                core: core.clone(),
                command,
                sequence,
            });
            intake.bytes = intake.bytes.saturating_add(bytes);
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
    fn retire(intake: &mut Intake, sequence: u64) -> Option<Record> {
        let record = intake.records.remove(&sequence)?;
        intake.bytes = intake.bytes.saturating_sub(record.bytes);
        if let Some(rows) = intake.org_rows.get_mut(&record.route) {
            *rows -= 1;
            if *rows == 0 {
                intake.org_rows.remove(&record.route);
            }
        }
        Some(record)
    }
    pub fn shutdown(
        self: &Arc<Self>,
        scope: &Arc<Observation>,
        timeout: std::time::Duration,
    ) -> Completion<ShutdownReport, ShutdownError> {
        let (sender, completion) = Completion::pair();
        let intake = self.intake.lock().unwrap();
        scope.state.lock().unwrap().closed = true;
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
        scope.state.lock().unwrap().terminal = Some(ExecutionError::TimedOut);
        let sequences: Vec<_> = intake
            .records
            .iter()
            .filter(|(_, record)| record.observer.includes(scope))
            .map(|(sequence, _)| *sequence)
            .collect();
        let error = WriteError::Execution(ExecutionError::TimedOut);
        let mut signals = Vec::new();
        let mut writes = Vec::new();
        for sequence in &sequences {
            if let Some(record) = Self::retire(&mut intake, *sequence) {
                record
                    .observer
                    .retire(*sequence, Some(&error), &mut signals);
                if let Some(sender) = record.sender {
                    writes.push(sender);
                }
            }
        }
        let live: HashSet<_> = intake.records.keys().copied().collect();
        let mut discarded = Vec::new();
        for entries in intake.partitions.values_mut() {
            let old = std::mem::take(entries);
            for entry in old {
                if live.contains(&entry.sequence) {
                    entries.push_back(entry);
                } else {
                    discarded.push(entry);
                }
            }
        }
        intake.partitions.retain(|_, entries| !entries.is_empty());
        drop(intake);
        for (sender, result) in signals {
            let _ = sender.send(result);
        }
        for sender in writes {
            let _ = sender.send(Err(error.clone()));
        }
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
    /// drops the task unpolled still requeues its rows.
    fn process_job(
        self: Arc<Self>,
        key: Key,
        entries: Vec<Entry>,
        runtime: Option<tokio::runtime::Id>,
    ) -> impl std::future::Future<Output = ()> + Send + 'static {
        let job = Job {
            report: Report::new(entries.len()),
            engine: self,
            runtime,
            key,
            entries,
        };
        job.run()
    }
}
impl Job {
    async fn run(mut self) {
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
            // Rows whose request succeeded were delivered; fail only the rest.
            job.report.settle_delivered();
            let error = anyhow::anyhow!("delivery partition panicked");
            for index in 0..count {
                if !job.report.settled[index] {
                    job.report.failed(vec![index], &error, true);
                }
            }
        }
        // A completed job has an outcome for every submission. Dropping the job
        // without reaching this point requeues whatever is still unsettled.
        job.report.settle(0..count);
    }
}

/// A job keeps its rows until each has an outcome. If the job is dropped first,
/// usually because its runtime shut down, unsettled rows go back in the queue.
struct Job {
    engine: Arc<Engine>,
    runtime: Option<tokio::runtime::Id>,
    key: Key,
    entries: Vec<Entry>,
    report: Report,
}
impl Drop for Job {
    fn drop(&mut self) {
        let entries = std::mem::take(&mut self.entries);
        let mut report = std::mem::replace(&mut self.report, Report::new(0));
        report.settle_delivered();
        if let (Some(runtime), true) = (self.runtime, report.settled.contains(&false)) {
            // Only a runtime shutting down drops an unfinished job. Move its
            // work now rather than spawning it there again.
            if let Some(service) = crate::service::started() {
                service.runtime_stopped(runtime);
            }
        }
        let engine = &self.engine;
        let mut intake = engine.intake.lock().unwrap_or_else(PoisonError::into_inner);
        intake.active.remove(&self.key);
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
        let mut signals = Vec::new();
        let mut writes = Vec::new();
        let mut requeue = Vec::new();
        let mut discarded = Vec::new();
        for ((entry, error), settled) in entries.into_iter().zip(report.errors).zip(report.settled)
        {
            if !settled {
                if intake.records.contains_key(&entry.sequence) {
                    requeue.push(entry);
                } else {
                    discarded.push(entry);
                }
                continue;
            }
            if let Some(record) = Engine::retire(&mut intake, entry.sequence) {
                let error = error.as_ref().map(|e| e.write_error());
                record
                    .observer
                    .retire(entry.sequence, error.as_ref(), &mut signals);
                if let Some(sender) = record.sender {
                    writes.push((sender, error.map_or(Ok(DeliveryReceipt::new()), Err)));
                }
            }
            discarded.push(entry);
        }
        if !requeue.is_empty() {
            // Ahead of rows accepted since, so the partition keeps submission order.
            let queue = intake.partitions.entry(self.key.clone()).or_default();
            for entry in requeue.into_iter().rev() {
                queue.push_front(entry);
            }
        }
        drop(intake);
        drop(discarded);
        for (sender, result) in signals {
            let _ = sender.send(result);
        }
        for (sender, result) in writes {
            let _ = sender.send(result);
        }
        engine.schedule(self.key.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log_queue::{LogQueue, LogQueueConfig};
    use crate::LoginState;
    fn queue() -> LogQueue {
        LogQueue::new(
            LogQueueConfig::builder().sync_flush(true).build(),
            LoginState::new(),
            crate::http::build_http_client(std::time::Duration::from_secs(1), None, None).unwrap(),
            32,
            "http://localhost".parse().unwrap(),
            "http://localhost".parse().unwrap(),
        )
    }
    #[test]
    fn global_limits_include_buffered_scopes_and_reject_only_incoming_rows() {
        let engine = Engine::new(ServiceConfig::builder().max_rows(2).build());
        let a = queue();
        let b = queue();
        engine.submit_many(
            a.core.clone(),
            [SubmitCommand::test_row("a1"), SubmitCommand::test_row("a2")],
        );
        engine.submit(b.core.clone(), SubmitCommand::test_row("b"));
        assert_eq!(engine.intake.lock().unwrap().records.len(), 2);
        assert_eq!(a.dropped_rows().queue_full, 0);
        assert_eq!(b.dropped_rows().queue_full, 1);
        assert_eq!(a.core.observation.len(), 2);
        assert_eq!(b.core.observation.len(), 0);
    }
    #[test]
    fn the_byte_limit_rejects_rows_that_would_exceed_it() {
        let engine = Engine::new(ServiceConfig::builder().max_bytes(1).build());
        let a = queue();
        engine.submit(a.core.clone(), SubmitCommand::test_row("oversized"));
        assert_eq!(engine.intake.lock().unwrap().bytes, 0);
        assert_eq!(a.dropped_rows().queue_full, 1);
    }
    #[tokio::test]
    async fn barriers_cover_only_work_accepted_before_them() {
        let engine = Engine::new(ServiceConfig::default());
        let a = queue();
        let b = queue();
        engine.submit(a.core.clone(), SubmitCommand::test_row("covered"));
        let barrier = engine.barrier(&a.core.observation);
        engine.submit(a.core.clone(), SubmitCommand::test_row("later"));
        engine.submit(b.core.clone(), SubmitCommand::test_row("unrelated"));
        let jobs = engine.take_jobs();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].1.len(), 3);
        assert!(engine.take_jobs().is_empty(), "the partition is in flight");
        a.core.observation.finish(jobs[0].1[0].sequence, None, 0, 0);
        assert_eq!(barrier.await.unwrap().delivered(), 1);
        assert_eq!(a.core.observation.len(), 1);
    }
    #[test]
    fn a_dropped_job_requeues_unsettled_rows_ahead_of_later_ones() {
        let engine = Arc::new(Engine::new(ServiceConfig::default()));
        let a = queue();
        engine.submit_many(
            a.core.clone(),
            [
                SubmitCommand::test_row("delivered"),
                SubmitCommand::test_row("in-flight"),
                SubmitCommand::test_row("unsent"),
            ],
        );
        let (key, entries) = engine.take_jobs().pop().unwrap();
        engine.submit(a.core.clone(), SubmitCommand::test_row("later"));
        let mut report = Report::new(entries.len());
        // Rows 0 and 1 went in one upload; the request holding row 0 succeeded
        // before the job was dropped, while row 1's was still in flight.
        report.uploads.push((
            vec![vec![0], vec![1]].into(),
            Arc::new(std::sync::atomic::AtomicUsize::new(1)),
        ));
        // As when the job's runtime shuts down mid-upload.
        drop(Job {
            engine: engine.clone(),
            runtime: None,
            key,
            entries,
            report,
        });
        assert_eq!(a.core.observation.len(), 3, "only the settled row retired");
        let jobs = engine.take_jobs();
        assert_eq!(jobs.len(), 1, "the partition is free again");
        let sequences: Vec<_> = jobs[0].1.iter().map(|entry| entry.sequence).collect();
        assert_eq!(sequences, [2, 3, 4]);
    }
    #[test]
    fn org_budgets_are_shared_across_facades() {
        let engine = Engine::new(ServiceConfig::builder().max_org_rows(3).build());
        let a = queue();
        let b = queue();
        engine.submit(
            a.core.clone(),
            SubmitCommand::test_scoped_row("a", "p", "org"),
        );
        engine.submit(
            b.core.clone(),
            SubmitCommand::test_scoped_row("same-project", "p", "org"),
        );
        engine.submit(
            b.core.clone(),
            SubmitCommand::test_scoped_row("other-project", "q", "org"),
        );
        engine.submit(
            b.core.clone(),
            SubmitCommand::test_scoped_row("over-org-budget", "r", "org"),
        );
        engine.submit(
            b.core.clone(),
            SubmitCommand::test_scoped_row("other-org", "p", "other"),
        );
        assert_eq!(b.dropped_rows().queue_full, 1);
        assert_eq!(engine.intake.lock().unwrap().records.len(), 4);
    }

    #[test]
    fn accepting_work_freezes_routing_before_later_login_discovery() {
        let engine = Engine::new(ServiceConfig::default());
        let queue = queue();
        engine.submit(queue.core.clone(), SubmitCommand::test_row("first"));
        queue.core.login_state.set(
            "token".into(),
            "org".into(),
            "name".into(),
            "http://discovered.example".into(),
            "http://localhost".into(),
        );
        let jobs = engine.take_jobs();
        assert_eq!(jobs[0].0.route.endpoint, "http://localhost/");
        assert_eq!(jobs[0].0.org_name, None);
    }
    fn http_queue(server: &wiremock::MockServer, max_items: usize) -> LogQueue {
        LogQueue::new(
            LogQueueConfig::builder()
                .sync_flush(true)
                .checked_delivery(true)
                .batch_max_items(max_items)
                .build(),
            LoginState::new(),
            crate::http::build_http_client(std::time::Duration::from_secs(30), None, None).unwrap(),
            32,
            server.uri().parse().unwrap(),
            server.uri().parse().unwrap(),
        )
    }

    #[tokio::test]
    async fn compatible_facades_share_uploads_with_independent_barriers() {
        use wiremock::{
            matchers::{method, path},
            Mock, MockServer, ResponseTemplate,
        };
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount(&server)
            .await;
        let engine = Arc::new(Engine::new(ServiceConfig::default()));
        let a = http_queue(&server, 100);
        let b = http_queue(&server, 100);
        engine.submit(a.core.clone(), SubmitCommand::test_row("a"));
        engine.submit(b.core.clone(), SubmitCommand::test_row("b"));
        let x = engine.barrier(&a.core.observation);
        let y = engine.barrier(&b.core.observation);
        let jobs = engine.take_jobs();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].1.len(), 2);
        for (key, entries) in jobs {
            engine.clone().process_job(key, entries, None).await;
        }
        x.await.unwrap();
        y.await.unwrap();
        let state = engine.intake.lock().unwrap();
        assert_eq!((state.records.len(), state.bytes), (0, 0));
        assert!(state.org_rows.is_empty() && state.active.is_empty());
    }

    #[tokio::test]
    async fn shared_split_failures_report_partial_delivery_to_each_owner() {
        use wiremock::{
            matchers::{method, path},
            Mock, MockServer, Request, ResponseTemplate,
        };
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(|request: &Request| {
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                if body["rows"].as_array().unwrap().len() > 1 {
                    ResponseTemplate::new(413)
                } else if body["rows"][0]["id"] == "a" {
                    ResponseTemplate::new(200).set_body_string("{}")
                } else {
                    ResponseTemplate::new(400)
                }
            })
            .mount(&server)
            .await;
        let engine = Arc::new(Engine::new(ServiceConfig::default()));
        let a = http_queue(&server, 100);
        let b = http_queue(&server, 100);
        engine.submit(a.core.clone(), SubmitCommand::test_row("a"));
        engine.submit(b.core.clone(), SubmitCommand::test_row("b"));
        let x = engine.barrier(&a.core.observation);
        let y = engine.barrier(&b.core.observation);
        for (key, entries) in engine.take_jobs() {
            engine.clone().process_job(key, entries, None).await;
        }
        assert!(x.await.is_err());
        assert!(y.await.is_err());
        assert_eq!(a.dropped_rows().upload_failed, 0);
        assert_eq!(b.dropped_rows().upload_failed, 1);
    }

    #[tokio::test]
    async fn failure_in_a_different_physical_batch_does_not_fail_a_successful_owner() {
        use wiremock::{
            matchers::{method, path},
            Mock, MockServer, Request, ResponseTemplate,
        };
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(|request: &Request| {
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                if body["rows"][0]["id"] == "a" {
                    ResponseTemplate::new(200).set_body_string("{}")
                } else {
                    ResponseTemplate::new(400)
                }
            })
            .mount(&server)
            .await;
        let engine = Arc::new(Engine::new(ServiceConfig::default()));
        let a = http_queue(&server, 1);
        let b = http_queue(&server, 1);
        engine.submit(a.core.clone(), SubmitCommand::test_row("a"));
        engine.submit(b.core.clone(), SubmitCommand::test_row("b"));
        let x = engine.barrier(&a.core.observation);
        let y = engine.barrier(&b.core.observation);
        for (key, entries) in engine.take_jobs() {
            engine.clone().process_job(key, entries, None).await;
        }
        x.await.unwrap();
        assert!(y.await.is_err());
        assert_eq!(a.dropped_rows().upload_failed, 0);
        assert_eq!(b.dropped_rows().upload_failed, 1);
    }
    #[tokio::test]
    async fn merged_rows_complete_all_owners_and_count_ancestor_loss_once() {
        use wiremock::{
            matchers::{method, path},
            Mock, MockServer, ResponseTemplate,
        };
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(400))
            .expect(1)
            .mount(&server)
            .await;
        let engine = Arc::new(Engine::new(ServiceConfig::default()));
        let root = http_queue(&server, 100);
        let a = root.child();
        let b = root.child();
        engine.submit(a.core.clone(), SubmitCommand::test_span(false));
        engine.submit(b.core.clone(), SubmitCommand::test_span(true));
        let x = engine.barrier(&a.core.observation);
        let y = engine.barrier(&b.core.observation);
        let z = engine.barrier(&root.core.observation);
        let jobs = engine.take_jobs();
        assert_eq!(jobs.len(), 1);
        for (key, entries) in jobs {
            engine.clone().process_job(key, entries, None).await;
        }
        assert!(x.await.is_err());
        assert!(y.await.is_err());
        assert!(z.await.is_err());
        assert_eq!(a.dropped_rows().upload_failed, 1);
        assert_eq!(b.dropped_rows().upload_failed, 1);
        assert_eq!(root.dropped_rows().upload_failed, 1);
    }
    #[tokio::test]
    async fn a_failed_project_registration_is_attempted_once_per_job() {
        use wiremock::{
            matchers::{method, path},
            Mock, MockServer, ResponseTemplate,
        };
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/project/register"))
            .respond_with(ResponseTemplate::new(403))
            .expect(1)
            .mount(&server)
            .await;
        let engine = Arc::new(Engine::new(ServiceConfig::default()));
        let queue = http_queue(&server, 100);
        for i in 0..20 {
            engine.submit(
                queue.core.clone(),
                SubmitCommand::test_named_span(&format!("row-{i}"), "forbidden"),
            );
        }
        let flush = engine.barrier(&queue.core.observation);
        let jobs = engine.take_jobs();
        assert_eq!(jobs.len(), 1);
        for (key, entries) in jobs {
            engine.clone().process_job(key, entries, None).await;
        }
        assert!(flush.await.is_err());
        assert_eq!(queue.dropped_rows().preparation_failed, 20);
    }

    #[tokio::test]
    async fn rotated_tokens_in_one_org_reuse_the_project_registration() {
        use wiremock::{
            matchers::{method, path},
            Mock, MockServer, ResponseTemplate,
        };
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/project/register"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"project":{"id":"resolved"}})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(2)
            .mount(&server)
            .await;
        let engine = Arc::new(Engine::new(ServiceConfig::default()));
        let queue = http_queue(&server, 100);
        for token in ["first-token", "rotated-token"] {
            let mut command = SubmitCommand::test_named_span(token, "rotation-project");
            command.credential = Credential::token_value(token.into());
            engine.submit(queue.core.clone(), command);
            let flush = engine.barrier(&queue.core.observation);
            for (key, entries) in engine.take_jobs() {
                engine.clone().process_job(key, entries, None).await;
            }
            flush.await.unwrap();
        }
    }

    #[tokio::test]
    async fn transient_project_registration_failures_are_retried() {
        use wiremock::{
            matchers::{method, path},
            Mock, MockServer, ResponseTemplate,
        };
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/project/register"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/project/register"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"project":{"id":"resolved"}})),
            )
            .with_priority(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount(&server)
            .await;
        let engine = Arc::new(Engine::new(ServiceConfig::default()));
        let queue = http_queue(&server, 100);
        engine.submit(
            queue.core.clone(),
            SubmitCommand::test_named_span("row", "flaky-project"),
        );
        let flush = engine.barrier(&queue.core.observation);
        for (key, entries) in engine.take_jobs() {
            engine.clone().process_job(key, entries, None).await;
        }
        assert_eq!(flush.await.unwrap().delivered(), 1);
    }

    #[test]
    fn racing_producers_share_one_bounded_admission_budget() {
        let engine = Arc::new(Engine::new(ServiceConfig::builder().max_rows(17).build()));
        let queue = queue();
        std::thread::scope(|threads| {
            for _ in 0..8 {
                let engine = engine.clone();
                let core = queue.core.clone();
                threads.spawn(move || {
                    for _ in 0..100 {
                        engine.submit(core.clone(), SubmitCommand::test_row("row"));
                    }
                });
            }
        });
        assert_eq!(engine.intake.lock().unwrap().records.len(), 17);
        assert_eq!(queue.core.observation.len(), 17);
        assert_eq!(queue.dropped_rows().queue_full, 783);
    }

    #[tokio::test]
    async fn completion_wakes_after_capacity_is_released_and_control_is_unlocked() {
        use futures::task::{waker_ref, ArcWake};
        use std::future::{Future, IntoFuture};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::task::{Context, Poll};
        use wiremock::{
            matchers::{method, path},
            Mock, MockServer, ResponseTemplate,
        };
        struct Reenter {
            engine: Arc<Engine>,
            core: Arc<LogQueueCore>,
            woke: AtomicBool,
        }
        impl ArcWake for Reenter {
            fn wake_by_ref(this: &Arc<Self>) {
                assert!(this.engine.intake.try_lock().is_ok());
                assert!(this
                    .engine
                    .submit(this.core.clone(), SubmitCommand::test_row("next"))
                    .submission()
                    .is_ok());
                this.woke.store(true, Ordering::SeqCst);
            }
        }
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        let queue = http_queue(&server, 1);
        let engine = Arc::new(Engine::new(ServiceConfig::builder().max_rows(1).build()));
        let write = engine.submit(queue.core.clone(), SubmitCommand::test_row("first"));
        let rejected = engine.submit(queue.core.clone(), SubmitCommand::test_row("rejected"));
        assert!(matches!(
            rejected.submission(),
            Err(WriteError::Admission(EnqueueError::Full))
        ));
        assert!(matches!(
            rejected.await,
            Err(WriteError::Admission(EnqueueError::Full))
        ));
        let signal = Arc::new(Reenter {
            engine: engine.clone(),
            core: queue.core.clone(),
            woke: AtomicBool::new(false),
        });
        let waker = waker_ref(&signal);
        let mut completion = Box::pin(write.into_future());
        assert!(matches!(
            completion.as_mut().poll(&mut Context::from_waker(&waker)),
            Poll::Pending
        ));
        let flush = engine.barrier(&queue.core.observation);
        let (key, entries) = engine.take_jobs().pop().unwrap();
        engine.clone().process_job(key, entries, None).await;
        completion.await.unwrap();
        assert!(signal.woke.load(Ordering::SeqCst));
        assert_eq!(flush.await.unwrap().delivered(), 1);
        assert_eq!(engine.intake.lock().unwrap().records.len(), 1);
    }
}
