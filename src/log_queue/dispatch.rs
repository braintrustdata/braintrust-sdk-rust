//! Preserve submission ownership through preparation, merging, batching, and split uploads.
use super::batching::build_overflow_row_from_logs3row;
use super::http::send_batch_counting;
use super::merge::merge_row_into;
use super::observation::DeliveryError;
use super::queue::{LogQueueCore, QueuedItem, SubmitCommand};
use super::row_key::RowKey;
use crate::{
    internal::ingest::{EncodedBatch, EncodedRow},
    types::Logs3Row,
    BraintrustError,
};
use futures::{stream, StreamExt};
use indexmap::IndexMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use url::Url;

pub(super) struct Loss {
    pub submissions: Vec<usize>,
    pub preparation: bool,
}
/// Outcomes recorded as a job progresses, so a job that is dropped part-way
/// can requeue exactly the submissions that have no outcome yet.
pub(super) struct Report {
    pub errors: Vec<Option<DeliveryError>>,
    pub losses: Vec<Loss>,
    pub settled: Vec<bool>,
    /// Batches being uploaded: each row's submissions in batch order, and how
    /// many leading rows have been delivered so far.
    pub uploads: Vec<(Vec<Vec<usize>>, Arc<AtomicUsize>)>,
}
impl Report {
    pub fn new(count: usize) -> Self {
        Self {
            errors: vec![None; count],
            losses: Vec::new(),
            settled: vec![false; count],
            uploads: Vec::new(),
        }
    }
    /// Settle rows whose request already succeeded, so a job dropped mid-upload
    /// does not send them again.
    pub fn settle_delivered(&mut self) {
        for (rows, delivered) in std::mem::take(&mut self.uploads) {
            let delivered = delivered.load(Ordering::Acquire);
            for submissions in rows.into_iter().take(delivered) {
                self.settle(submissions);
            }
        }
    }
    pub fn settle(&mut self, submissions: impl IntoIterator<Item = usize>) {
        for index in submissions {
            self.settled[index] = true;
        }
    }
    pub fn failed(&mut self, submissions: Vec<usize>, error: &anyhow::Error, preparation: bool) {
        let error = DeliveryError::from_error(error);
        for index in &submissions {
            self.errors[*index] = Some(error.clone());
            self.settled[*index] = true;
        }
        self.losses.push(Loss {
            submissions,
            preparation,
        });
    }
}

impl LogQueueCore {
    async fn get_version_info(
        &self,
        api_url: &Url,
        token: &str,
        org_name: Option<&str>,
    ) -> (usize, bool) {
        let service = crate::service::global().expect("initialized service");
        let key = crate::resolution::CapabilityKey {
            api_url: api_url.to_string(),
        };
        // Capabilities belong to the server, and every org waits for the first
        // lookup, so it must not wait out one org's cooldown.
        let scope = crate::scheduler::Scope::new(api_url.as_str(), "");
        let cell = service.capabilities.entry(key);
        *cell
            .get_or_try_init(|| async {
                let api = crate::http::api_client(
                    &self.client,
                    api_url.as_str(),
                    self.app_url.as_str(),
                    token,
                    org_name,
                )?;
                let version = crate::internal::upload::scheduled(scope, 0, || api.version())
                    .await?
                    .into_inner();
                let bytes = version.logs3_payload_max_bytes.filter(|n| *n > 0);
                Ok::<_, BraintrustError>((
                    bytes.unwrap_or(super::config::DEFAULT_BATCH_MAX_BYTES * 2),
                    bytes.is_some(),
                ))
            })
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(%error,"failed to fetch ingestion capabilities");
                &(super::config::DEFAULT_BATCH_MAX_BYTES * 2, false)
            })
    }

    /// The engine seals a compatible partition before assigning local submission indices.
    /// Commands are borrowed so the engine can requeue them if this job is
    /// dropped before every submission is settled in `report`.
    pub(super) async fn dispatch(
        self: &Arc<Self>,
        commands: &[&SubmitCommand],
        report: &mut Report,
    ) {
        if commands.is_empty() {
            return;
        }
        let (credential, _, org_name) = commands[0].partition();
        let org_name = org_name.or_else(|| self.routing_org_name.clone());
        let api_url = match Url::parse(&self.endpoint()) {
            Ok(url) => url,
            Err(error) => {
                let error = anyhow::Error::from(error);
                for i in 0..commands.len() {
                    report.failed(vec![i], &error, false);
                }
                return;
            }
        };
        let service = crate::service::global().expect("initialized service");
        // One token serves the whole job: preparation, registration, and uploads.
        // A provider can fail transiently, so it is retried like an upload; the
        // wait between attempts holds no preparation permit.
        let mut delays = crate::internal::upload::queue_delays(self.config.num_retries());
        let token = loop {
            let fetched = {
                let _permit = service
                    .preparations
                    .acquire()
                    .await
                    .expect("service stays open");
                credential.token().await
            };
            match (fetched, delays.next()) {
                (Ok(token), _) => break token,
                (Err(error), Some(delay)) => {
                    tracing::warn!(%error, ?delay, "failed to get upload token, retrying");
                    tokio::time::sleep(delay).await;
                }
                (Err(error), None) => {
                    tracing::warn!(%error, "failed to get upload token, dropping rows");
                    for i in 0..commands.len() {
                        report.failed(vec![i], &error, false);
                    }
                    return;
                }
            }
        };
        let permit = service
            .preparations
            .acquire()
            .await
            .expect("service stays open");
        let mut failures = super::prepare::ProjectFailures::new();
        let mut spans: IndexMap<RowKey, (Logs3Row, Vec<usize>)> = IndexMap::new();
        let mut raw = Vec::new();
        for (index, command) in commands.iter().enumerate() {
            if index % 64 == 0 {
                tokio::task::yield_now().await;
            }
            match &command.item {
                QueuedItem::Span {
                    payload,
                    parent_info,
                } => {
                    let result = self
                        .prepare_span(&token, payload.clone(), parent_info.clone(), &mut failures)
                        .await;
                    match result {
                        Ok(row) => {
                            let key = RowKey::from_row(&row);
                            if let Some((existing, submissions)) = spans.get_mut(&key) {
                                if row.is_merge.unwrap_or(false) {
                                    merge_row_into(existing, row);
                                } else {
                                    *existing = row;
                                }
                                submissions.push(index);
                            } else {
                                spans.insert(key, (row, vec![index]));
                            }
                        }
                        Err(error) => {
                            tracing::warn!(%error, "failed to prepare span, dropping");
                            report.failed(vec![index], &error, true);
                        }
                    }
                }
                QueuedItem::Row { row, .. } => raw.push((index, row.clone())),
            }
        }
        drop(permit);
        if spans.is_empty() && raw.is_empty() {
            return;
        }
        let (max_request_size, overflow) = self
            .get_version_info(&api_url, &token, org_name.as_deref())
            .await;
        let max_bytes = self.config.batch_max_bytes().min(max_request_size / 2);
        let max_items = self.config.batch_max_items();
        let encoded = tokio::task::spawn_blocking(move || {
            let mut encoded = Vec::new();
            let mut failures = Vec::new();
            for (row, submissions) in spans.into_values() {
                match EncodedRow::encode(&row, build_overflow_row_from_logs3row(&row)) {
                    Ok(row) => encoded.push(row.with_submissions(submissions)),
                    Err(error) => failures.push((submissions, anyhow::Error::from(error))),
                }
            }
            for (index, row) in raw {
                match EncodedRow::from_map(&row) {
                    Ok(row) => encoded.push(row.with_submissions(vec![index])),
                    Err(error) => failures.push((vec![index], anyhow::Error::from(error))),
                }
            }
            (
                EncodedBatch::partition_by_destination(encoded, max_items, max_bytes),
                failures,
            )
        })
        .await;
        let (batches, failures) = match encoded {
            Ok(encoded) => encoded,
            Err(error) => {
                let error = anyhow::Error::from(error);
                for i in 0..report.errors.len() {
                    if report.errors[i].is_none() {
                        report.failed(vec![i], &error, true);
                    }
                }
                return;
            }
        };
        for (submissions, error) in failures {
            report.failed(submissions, &error, true);
        }
        let batches: Vec<_> = batches
            .into_iter()
            .map(|batch| {
                let delivered = Arc::new(AtomicUsize::new(0));
                report
                    .uploads
                    .push((batch.submissions(), delivered.clone()));
                (batch, delivered)
            })
            .collect();
        let sends = batches.into_iter().map(|(batch, delivered)| {
            let api_url = &api_url;
            let token = token.as_ref();
            let org_name = org_name.as_deref();
            async move {
                let submissions = batch.submissions();
                let result = send_batch_counting(
                    &self.client,
                    api_url,
                    token,
                    org_name,
                    batch,
                    &self.config,
                    max_request_size,
                    overflow,
                    &delivered,
                )
                .await;
                (submissions, delivered.load(Ordering::Acquire), result)
            }
        });
        let mut sends = stream::iter(sends).buffer_unordered(usize::MAX);
        while let Some((rows, delivered, result)) = sends.next().await {
            report.settle(rows.iter().flatten().copied());
            if let Err(error) = result {
                tracing::warn!(%error,"destination upload failed");
                let failure = DeliveryError::from_error(&error);
                // A split may have delivered a prefix. The whole logical batch fails
                // its checked observers, but only undelivered rows count as lost.
                for (position, submissions) in rows.into_iter().enumerate() {
                    for index in &submissions {
                        report.errors[*index] = Some(failure.clone());
                    }
                    if position >= delivered {
                        report.losses.push(Loss {
                            submissions,
                            preparation: false,
                        });
                    }
                }
            }
        }
    }
}
