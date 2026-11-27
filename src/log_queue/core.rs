use super::config::LogQueueConfig;
use super::http::save_payload_debug;
use super::prepare::SpanRoute;
use super::submission::{QueuedItem, SubmitCommand};
use crate::logger::LoginState;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::warn;
use url::Url;

/// Settings shared by a client and its derived handles.
pub(crate) struct QueueSettings {
    pub(crate) config: Arc<LogQueueConfig>,
    pub(crate) client: crate::http::SharedHttp,
    pub(crate) api_url: Url,
    pub(crate) app_url: Url,
}

pub(crate) struct LogQueueCore {
    pub(crate) settings: Arc<QueueSettings>,
    pub(crate) observation: Arc<super::observation::Observation>,
    dropped_count: AtomicUsize,
    last_drop_log_time: AtomicU64,
    queue_depth_warning_emitted: AtomicBool,
    pub(super) login_state: LoginState,
}

impl LogQueueCore {
    pub(super) fn new(
        settings: Arc<QueueSettings>,
        login_state: LoginState,
        parent: Option<Arc<super::observation::Observation>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            settings,
            login_state,
            observation: Arc::new(super::observation::Observation::new(parent)),
            dropped_count: AtomicUsize::new(0),
            last_drop_log_time: AtomicU64::new(0),
            queue_depth_warning_emitted: AtomicBool::new(false),
        })
    }

    pub(crate) fn route(&self, org_id: &str, org_name: Option<&str>) -> (String, Option<String>) {
        let same_org = self.login_state.org_id().as_deref() == Some(org_id)
            || (org_id.is_empty()
                && org_name.is_some()
                && self.login_state.org_name().as_deref() == org_name);
        let endpoint = same_org
            .then(|| self.login_state.api_url())
            .flatten()
            .unwrap_or_else(|| self.settings.api_url.to_string());
        let name = org_name
            .map(str::to_owned)
            .or_else(|| same_org.then(|| self.login_state.org_name()).flatten());
        (endpoint, name)
    }

    pub(super) fn push(
        self: &Arc<Self>,
        cmd: SubmitCommand,
    ) -> crate::Completion<crate::DeliveryReceipt, crate::WriteError> {
        crate::service::global()
            .expect("initialized service")
            .engine
            .submit(self.clone(), cmd)
    }

    pub(crate) fn rejected(&self, cmd: SubmitCommand) {
        self.record_full(1);
        self.dump_dropped_row_if_configured(cmd);
    }

    pub(crate) fn record_full(&self, count: usize) {
        self.dropped_count.fetch_add(count, Ordering::Relaxed);
        for _ in 0..count {
            self.observation.queue_full();
        }
        self.log_drop_warning();
    }

    pub(crate) fn retained(&self, rows: usize) {
        if rows == 1 {
            self.queue_depth_warning_emitted
                .store(false, Ordering::Relaxed);
        }
        self.log_queue_depth_warning(rows);
    }

    /// Log a drop warning with throttling.
    ///
    /// Reports the number of items dropped since the last warning was emitted,
    /// then resets the counter. Throttled to at most once per `queue_drop_logging_period`.
    fn log_drop_warning(&self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let last_log = self.last_drop_log_time.load(Ordering::Relaxed);
        let throttle_secs = self.settings.config.queue_drop_logging_period().as_secs();

        if now - last_log >= throttle_secs
            && self
                .last_drop_log_time
                .compare_exchange(last_log, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            let count = self.dropped_count.swap(0, Ordering::Relaxed);
            if count > 0 {
                warn!(
                    "Log queue full (size: {}), dropped {} items since last warning. \
                     Consider increasing BRAINTRUST_QUEUE_DROP_EXCEEDING_MAXSIZE.",
                    self.settings.config.queue_max_size(),
                    count
                );
            }
        }
    }

    /// Warn once per fill cycle when the queue reaches 80% of its configured maximum.
    fn log_queue_depth_warning(&self, queue_size: usize) {
        let queue_max_size = self.settings.config.queue_max_size();
        let warning_threshold = queue_max_size.saturating_mul(4).div_ceil(5);

        if queue_size >= warning_threshold
            && !self
                .queue_depth_warning_emitted
                .swap(true, Ordering::Relaxed)
        {
            warn!(
                queue_size,
                queue_max_size, "Log queue has reached 80% of the configured maximum depth"
            );
        }
    }

    /// If debug directories are configured, serialize the dropped command's payload
    /// and write it to disk. Spawns a background task — best-effort, non-blocking.
    fn dump_dropped_row_if_configured(&self, cmd: SubmitCommand) {
        let all_dir = self.settings.config.all_publish_payloads_dir();
        let failed_dir = self.settings.config.failed_publish_payloads_dir();
        let dirs: Vec<_> = [all_dir, failed_dir].into_iter().flatten().collect();
        if dirs.is_empty() {
            return;
        }
        let service = crate::service::global().expect("initialized service");
        service.spawn_operation(async move {
            let SubmitCommand {
                credential: _,
                item,
            } = cmd;
            let (payload, parent_info) = match item {
                QueuedItem::Span {
                    payload,
                    parent_info,
                } => (payload, parent_info),
                QueuedItem::Row { row, .. } => {
                    write_dropped_row_debug(dirs, &row);
                    return;
                }
            };

            let Ok(route) = SpanRoute::new(&payload, parent_info.as_ref()) else {
                return;
            };
            let Some(row) = route.into_known_row(payload) else {
                return;
            };
            write_dropped_row_debug(dirs, &row);
        });
    }
}

/// Write a dropped row to each debug directory as a one-row logs3 payload.
fn write_dropped_row_debug(dirs: Vec<std::path::PathBuf>, row: &impl serde::Serialize) {
    let payload_bytes = match serde_json::to_vec(&serde_json::json!({
        "rows": [row],
        "api_version": crate::types::LOGS_API_VERSION,
    })) {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "failed to serialize dropped row for debug dump");
            return;
        }
    };
    for dir in dirs {
        if let Err(e) = save_payload_debug(&payload_bytes, Some(dir), "dropped") {
            warn!(error = %e, "failed to write dropped row to debug dir");
        }
    }
}
