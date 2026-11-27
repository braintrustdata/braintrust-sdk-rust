//! Preserve submission ownership through preparation, merging, batching, and split uploads.
use super::job::Job;
use crate::internal::request::{RequestGuard, REQUEST_GUARD};
use crate::log_queue::batching::build_overflow_row_from_logs3row;
use crate::log_queue::http::send_batch_counting;
use crate::log_queue::merge::merge_row_into;
use crate::log_queue::observation::DeliveryError;
use crate::log_queue::report::Loss;
use crate::log_queue::row_key::RowKey;
use crate::log_queue::submission::QueuedItem;
use crate::{
    internal::{
        ingest::{EncodedBatch, EncodedRow},
        upload::Capabilities,
    },
    types::Logs3Row,
};
use futures::{stream, StreamExt};
use indexmap::IndexMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use url::Url;

impl Job {
    pub(super) async fn dispatch(&mut self) {
        self.discard_expired();
        let Some(entry) = self.entries.iter().flatten().next() else {
            return;
        };
        let settings = entry.core.settings.clone();
        let credential = entry.command.credential.clone();
        let endpoint = self.key.route.endpoint.clone();
        let org_name = self.key.org_name.clone();
        let count = self.entries.len();
        let api_url = match Url::parse(&endpoint) {
            Ok(url) => url,
            Err(error) => {
                let error = DeliveryError::new(error.into());
                for i in 0..count {
                    self.report.failed(vec![i], &error, false);
                }
                return;
            }
        };
        // One token serves the whole job: preparation, registration, and uploads.
        // A provider can fail transiently, so it is retried like an upload.
        let mut delays = crate::internal::request::queue_delays(settings.config.num_retries());
        let token = loop {
            match (credential.token().await, delays.next()) {
                (Ok(token), _) => break token,
                (Err(error), Some(delay)) => {
                    tracing::warn!(%error, ?delay, "failed to get upload token, retrying");
                    tokio::time::sleep(delay).await;
                }
                (Err(error), None) => {
                    tracing::warn!(%error, "failed to get upload token, dropping rows");
                    let error = DeliveryError::new(error);
                    for i in 0..count {
                        self.report.failed(vec![i], &error, false);
                    }
                    return;
                }
            }
        };
        self.discard_expired();
        let project = crate::log_queue::prepare::ProjectResolution::new();
        let mut spans: IndexMap<RowKey, (Logs3Row, Vec<usize>)> = IndexMap::new();
        let mut raw = Vec::new();
        for (index, entry) in self.entries.iter().enumerate() {
            let Some(entry) = entry else {
                continue;
            };
            if index % 64 == 0 {
                tokio::task::yield_now().await;
            }
            match &entry.command.item {
                QueuedItem::Span {
                    payload,
                    parent_info,
                } => {
                    let result = settings
                        .prepare_span(&token, payload.clone(), parent_info.clone(), &project)
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
                            tracing::warn!(?error, "failed to prepare span, dropping");
                            self.report.failed(vec![index], &error, true);
                        }
                    }
                }
                QueuedItem::Row { row, .. } => raw.push((index, row.clone())),
            }
        }
        if spans.is_empty() && raw.is_empty() {
            return;
        }
        let client = crate::http::api_client(
            &settings.client.client(),
            &endpoint,
            settings.app_url.as_str(),
            &token,
            org_name.as_deref(),
        );
        let Capabilities {
            max_request_size,
            overflow,
        } = Capabilities::resolve(
            client,
            crate::log_queue::config::DEFAULT_BATCH_MAX_BYTES * 2,
            None,
        )
        .await;
        let max_bytes = settings.config.batch_max_bytes().min(max_request_size / 2);
        let max_items = settings.config.batch_max_items();
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
                let error = DeliveryError::new(error.into());
                for i in 0..self.report.outcomes.len() {
                    if self.entries[i].is_some() {
                        self.report.failed(vec![i], &error, true);
                    }
                }
                return;
            }
        };
        for (submissions, error) in failures {
            self.report
                .failed(submissions, &DeliveryError::new(error), true);
        }
        self.retire_ready();
        let batches: Vec<_> = batches
            .into_iter()
            .map(|mut batch| {
                let delivered = Arc::new(AtomicUsize::new(0));
                let submissions: Arc<[_]> = batch.take_submissions().into();
                self.report
                    .uploads
                    .push((submissions.clone(), delivered.clone()));
                let sequences: Vec<_> = submissions
                    .iter()
                    .flatten()
                    .filter_map(|&index| self.entries[index].as_ref().map(|entry| entry.sequence))
                    .collect();
                let engine = self.engine.clone();
                let guard = Arc::new(RequestGuard::new(move || {
                    let intake = engine.intake.lock().unwrap();
                    sequences.iter().any(|id| !intake.records.contains_key(id))
                }));
                (batch, submissions, delivered, guard)
            })
            .collect();
        let sends = batches
            .into_iter()
            .map(|(batch, submissions, delivered, guard)| {
                let api_url = &api_url;
                let token = token.as_ref();
                let settings = &settings;
                let org_name = org_name.as_deref();
                async move {
                    let scope = crate::scheduler::SCOPE
                        .with(|scope| scope.for_destination(batch.destination()));
                    let client = settings.client.client();
                    let result = REQUEST_GUARD
                        .scope(
                            guard.clone(),
                            crate::scheduler::SCOPE.scope(
                                scope,
                                send_batch_counting(
                                    &client,
                                    api_url,
                                    token,
                                    org_name,
                                    batch,
                                    &settings.config,
                                    max_request_size,
                                    overflow,
                                    &delivered,
                                ),
                            ),
                        )
                        .await;
                    (
                        submissions,
                        delivered.load(Ordering::Acquire),
                        result,
                        guard.stopped(),
                    )
                }
            });
        // Keep only a bounded number of requests from this partition waiting
        // for permits, even when its backlog splits into many batches.
        let concurrency = crate::service::config().max_concurrent_requests;
        let mut sends = stream::iter(sends).buffer_unordered(concurrency);
        while let Some((rows, delivered, result, cancelled)) = sends.next().await {
            if cancelled {
                self.retry_cancelled = true;
                self.report
                    .settle(rows.iter().take(delivered).flatten().copied());
            } else if let Err(error) = result {
                tracing::warn!(%error,"destination upload failed");
                let failure = DeliveryError::new(error);
                // A split may have delivered a prefix. The whole logical batch fails
                // its checked observers, but only undelivered rows count as lost.
                for (position, submissions) in rows.iter().enumerate() {
                    self.report.fail_submissions(submissions, &failure);
                    if position >= delivered {
                        self.report.losses.push(Loss {
                            submissions: submissions.clone(),
                            preparation: false,
                        });
                    }
                }
            } else {
                self.report.settle(rows.iter().flatten().copied());
            }
            self.retire_ready();
        }
    }
}
