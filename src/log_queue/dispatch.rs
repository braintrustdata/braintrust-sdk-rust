//! Preserve submission ownership through preparation, merging, batching, and split uploads.
use super::batching::build_overflow_row_from_logs3row;
use super::core::QueueSettings;
use super::http::send_batch_counting;
use super::merge::merge_row_into;
use super::observation::DeliveryError;
use super::report::{Loss, Outcome, Report};
use super::row_key::RowKey;
use super::submission::{QueuedItem, SubmitCommand};
use crate::{
    internal::{
        ingest::{EncodedBatch, EncodedRow},
        upload::Capabilities,
    },
    types::Logs3Row,
};
use futures::{stream, StreamExt};
use indexmap::IndexMap;
use url::Url;

impl QueueSettings {
    /// The engine seals a compatible partition before assigning local submission indices.
    /// Commands are borrowed so the engine can requeue them if this job is
    /// dropped before every submission is settled in `report`.
    pub(super) async fn dispatch(
        &self,
        commands: &[&SubmitCommand],
        report: &mut Report,
        endpoint: &str,
        org_name: Option<&str>,
    ) {
        if commands.is_empty() {
            return;
        }
        let credential = &commands[0].credential;
        let api_url = match Url::parse(endpoint) {
            Ok(url) => url,
            Err(error) => {
                let error = anyhow::Error::from(error);
                for i in 0..commands.len() {
                    report.failed(vec![i], &error, false);
                }
                return;
            }
        };
        // One token serves the whole job: preparation, registration, and uploads.
        // A provider can fail transiently, so it is retried like an upload.
        let mut delays = crate::internal::request::queue_delays(self.config.num_retries());
        let token = loop {
            match (credential.token().await, delays.next()) {
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
        let project = super::prepare::ProjectResolution::new();
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
                            tracing::warn!(%error, "failed to prepare span, dropping");
                            report.failed(vec![index], &error, true);
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
            &self.client.client(),
            endpoint,
            self.app_url.as_str(),
            &token,
            org_name,
        );
        let Capabilities {
            max_request_size,
            overflow,
        } = Capabilities::resolve(client, super::config::DEFAULT_BATCH_MAX_BYTES * 2, None).await;
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
                for i in 0..report.outcomes.len() {
                    if matches!(report.outcomes[i], Outcome::Pending) {
                        report.failed(vec![i], &error, true);
                    }
                }
                return;
            }
        };
        for (submissions, error) in failures {
            report.failed(submissions, &error, true);
        }
        let sends = batches.into_iter().map(|mut batch| {
            let submissions = batch.take_submissions();
            let api_url = &api_url;
            let token = token.as_ref();
            async move {
                let scope = crate::scheduler::SCOPE
                    .with(|scope| scope.for_destination(batch.destination()));
                let (delivered, result) = crate::scheduler::SCOPE
                    .scope(
                        scope,
                        send_batch_counting(
                            &self.client.client(),
                            api_url,
                            token,
                            org_name,
                            batch,
                            &self.config,
                            max_request_size,
                            overflow,
                        ),
                    )
                    .await;
                (submissions, delivered, result)
            }
        });
        // Keep only a bounded number of requests from this partition waiting
        // for permits, even when its backlog splits into many batches.
        let concurrency = crate::service::config().max_concurrent_requests;
        let mut sends = stream::iter(sends).buffer_unordered(concurrency);
        while let Some((rows, delivered, result)) = sends.next().await {
            report.settle(rows.iter().flatten().copied());
            if let Err(error) = result {
                tracing::warn!(%error,"destination upload failed");
                let failure = DeliveryError::from_error(&error);
                // A split may have delivered a prefix. The whole logical batch fails
                // its checked observers, but only undelivered rows count as lost.
                for (position, submissions) in rows.iter().enumerate() {
                    report.fail_submissions(submissions, &failure);
                    if position >= delivered {
                        report.losses.push(Loss {
                            submissions: submissions.clone(),
                            preparation: false,
                        });
                    }
                }
            }
        }
    }
}
