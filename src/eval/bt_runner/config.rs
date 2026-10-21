//! Capture and validate CLI settings before admitting initialization.
use crate::eval::BtInitError;
use serde::Deserialize;

pub(super) struct EvalFilter {
    pub path: Vec<String>,
    pub pattern: regex::Regex,
}
impl EvalFilter {
    pub fn matches(&self, name: &str) -> bool {
        if self.path.is_empty() || self.path == ["name"] {
            self.pattern.is_match(name)
        } else {
            true
        } // Preserve existing handling of unsupported CLI filter paths.
    }
}
#[derive(Clone)]
pub(super) enum SseEndpoint {
    #[cfg(unix)]
    Unix(std::path::PathBuf),
    Tcp(String),
}
pub(super) struct RunnerConfig {
    pub jsonl: bool,
    pub list: bool,
    pub terminate_on_failure: bool,
    pub filters: Vec<EvalFilter>,
    pub no_send_logs: bool,
    pub endpoint: Option<SseEndpoint>,
}
impl RunnerConfig {
    pub fn from_env() -> Result<Self, BtInitError> {
        let list = env_flag("BT_EVAL_LIST");
        // List output is stdout-only and requires no socket or runtime setup.
        let mut endpoint = None;
        if !list {
            #[cfg(unix)]
            if let Some(path) = std::env::var_os("BT_EVAL_SSE_SOCK").filter(|s| !s.is_empty()) {
                endpoint = Some(SseEndpoint::Unix(path.into()));
            }
            if endpoint.is_none() {
                if let Some(address) = std::env::var("BT_EVAL_SSE_ADDR")
                    .ok()
                    .filter(|s| !s.is_empty())
                {
                    endpoint = Some(SseEndpoint::Tcp(address));
                }
            }
        }
        Ok(Self {
            jsonl: env_flag("BT_EVAL_JSONL"),
            list,
            terminate_on_failure: env_flag("BT_EVAL_TERMINATE_ON_FAILURE"),
            filters: parse_filters(std::env::var("BT_EVAL_FILTER_PARSED").ok().as_deref())?,
            no_send_logs: env_flag("BT_EVAL_NO_SEND_LOGS") || env_flag("BT_EVAL_LOCAL"),
            endpoint,
        })
    }
}
pub(super) fn env_flag(name: &str) -> bool {
    !matches!(
        std::env::var(name).ok().as_deref(),
        None | Some("") | Some("0") | Some("false") | Some("no") | Some("off")
    )
}
pub(super) fn parse_filters(serialized: Option<&str>) -> Result<Vec<EvalFilter>, BtInitError> {
    let Some(serialized) = serialized.filter(|s| !s.is_empty()) else {
        return Ok(vec![]);
    };
    #[derive(Deserialize)]
    struct Filter {
        path: Vec<String>,
        pattern: String,
    }
    let filters: Vec<Filter> = serde_json::from_str(serialized).map_err(BtInitError::Filters)?;
    filters
        .into_iter()
        .enumerate()
        .map(|(index, filter)| {
            let pattern = regex::Regex::new(&filter.pattern)
                .map_err(|source| BtInitError::FilterPattern { index, source })?;
            Ok(EvalFilter {
                path: filter.path,
                pattern,
            })
        })
        .collect()
}
