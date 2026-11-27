use super::config::LogQueueConfig;
use super::core::{LogQueueCore, QueueSettings};
use super::credential::{Credential, TokenProvider};
use super::observation::DroppedRows;
use super::submission::{QueuedItem, SubmitCommand};
use crate::logger::LoginState;
use crate::types::{ParentSpanInfo, SpanPayload};
use serde_json::{Map, Value};
use std::sync::Arc;
use url::Url;

/// A scoped facade over the process-wide delivery service.
#[derive(Clone)]
pub struct LogQueue {
    pub(crate) core: Arc<LogQueueCore>,
}

impl LogQueue {
    pub fn new(
        config: LogQueueConfig,
        login_state: LoginState,
        client: crate::http::SharedHttp,
        _worker_queue_size: usize,
        api_url: Url,
        app_url: Url,
    ) -> Self {
        Self {
            core: LogQueueCore::new(
                Arc::new(QueueSettings {
                    config: Arc::new(config),
                    client,
                    api_url,
                    app_url,
                }),
                login_state,
            ),
        }
    }

    pub(crate) fn submit(
        &self,
        credential: Credential,
        payload: SpanPayload,
        parent_info: Option<ParentSpanInfo>,
    ) -> crate::Completion<crate::DeliveryReceipt, crate::WriteError> {
        self.core.push(SubmitCommand {
            credential,
            item: QueuedItem::Span {
                payload,
                parent_info,
            },
        })
    }

    pub(crate) fn submit_rows(
        &self,
        token: &str,
        org_id: &str,
        org_name: Option<&str>,
        rows: impl IntoIterator<Item = Map<String, Value>>,
    ) {
        self.submit_rows_with(
            Credential::token_value(token.into()),
            org_id,
            org_name,
            rows,
        );
    }
    pub(crate) fn submit_rows_tracked(
        &self,
        token: &str,
        org_id: &str,
        org_name: Option<&str>,
        rows: Vec<Map<String, Value>>,
    ) -> Vec<crate::Completion<crate::DeliveryReceipt, crate::WriteError>> {
        let credential = Credential::token_value(token.to_owned());
        rows.into_iter()
            .map(|row| {
                self.core.push(SubmitCommand {
                    credential: credential.clone(),
                    item: QueuedItem::Row {
                        org_id: org_id.to_owned(),
                        org_name: org_name.map(str::to_owned),
                        row,
                    },
                })
            })
            .collect()
    }

    pub(crate) fn submit_rows_with_provider(
        &self,
        provider: Arc<dyn TokenProvider>,
        org_id: &str,
        org_name: Option<&str>,
        rows: impl IntoIterator<Item = Map<String, Value>>,
    ) {
        self.submit_rows_with(Credential::Provider(provider), org_id, org_name, rows);
    }
    fn submit_rows_with(
        &self,
        credential: Credential,
        org_id: &str,
        org_name: Option<&str>,
        rows: impl IntoIterator<Item = Map<String, Value>>,
    ) {
        // Never run caller iterator code while holding the shared intake lock.
        let mut rows = rows.into_iter();
        loop {
            let commands: Vec<_> = rows
                .by_ref()
                .take(1024)
                .map(|row| SubmitCommand {
                    credential: credential.clone(),
                    item: QueuedItem::Row {
                        org_id: org_id.into(),
                        org_name: org_name.map(str::to_owned),
                        row,
                    },
                })
                .collect();
            if commands.is_empty() {
                break;
            }
            crate::service::global()
                .expect("initialized service")
                .engine
                .submit_many(self.core.clone(), commands);
        }
    }
    pub(crate) fn dropped_rows(&self) -> DroppedRows {
        self.core.observation.dropped_rows()
    }
    pub fn flush(&self) -> crate::Completion<crate::FlushReport, crate::FlushError> {
        crate::service::global()
            .expect("initialized service")
            .engine
            .barrier(&self.core.observation)
    }
    pub fn shutdown(
        &self,
        timeout: std::time::Duration,
    ) -> crate::Completion<crate::ShutdownReport, crate::ShutdownError> {
        crate::service::global()
            .expect("initialized service")
            .engine
            .shutdown(&self.core.observation, timeout)
    }
}
