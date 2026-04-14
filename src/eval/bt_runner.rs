//! Runner designed for use with the `bt eval` CLI.
//!
//! This module provides [`BtEvalRunner`], which reads configuration from environment
//! variables set by the `bt eval` CLI and handles SSE output, list mode, filtering,
//! and other CLI-driven behaviors.
//!
//! See the [bt CLI](https://github.com/braintrustdata/bt) for more information on
//! how these environment variables are used.

use std::io::Write;
use std::sync::{Arc, Mutex};

use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};

use crate::{BraintrustClient, BraintrustError, Result};

use super::{
    runner::{EvalOpts, Evaluator},
    types::EvalSummary,
};

/// A filter for selecting which evaluators to run, read from `BT_EVAL_FILTER_PARSED`.
struct EvalFilter {
    path: Vec<String>,
    pattern: regex::Regex,
}

impl EvalFilter {
    /// Returns true if the given eval name matches this filter.
    fn matches(&self, name: &str) -> bool {
        // Match against the eval name when path is ["name"] or empty
        if self.path.is_empty() || self.path == ["name"] {
            return self.pattern.is_match(name);
        }
        // Unknown path — include by default
        true
    }
}

/// Configuration read from `BT_EVAL_*` environment variables.
struct RunnerConfig {
    /// Output summaries as JSONL to stdout (`BT_EVAL_JSONL=1`)
    jsonl: bool,
    /// List evaluator names without running them (`BT_EVAL_LIST=1`)
    list: bool,
    /// Stop running after the first failure (`BT_EVAL_TERMINATE_ON_FAILURE=1`)
    terminate_on_failure: bool,
    /// Filters to select which evaluators to run (`BT_EVAL_FILTER_PARSED`)
    filters: Vec<EvalFilter>,
    /// Skip sending logs to Braintrust (`BT_EVAL_NO_SEND_LOGS=1` or `BT_EVAL_LOCAL=1`)
    no_send_logs: bool,
}

impl RunnerConfig {
    fn from_env() -> Self {
        Self {
            jsonl: env_flag("BT_EVAL_JSONL"),
            list: env_flag("BT_EVAL_LIST"),
            terminate_on_failure: env_flag("BT_EVAL_TERMINATE_ON_FAILURE"),
            filters: parse_filters(),
            no_send_logs: env_flag("BT_EVAL_NO_SEND_LOGS") || env_flag("BT_EVAL_LOCAL"),
        }
    }
}

fn env_flag(name: &str) -> bool {
    !matches!(
        std::env::var(name).ok().as_deref(),
        None | Some("") | Some("0") | Some("false") | Some("no") | Some("off")
    )
}

fn parse_filters() -> Vec<EvalFilter> {
    let serialized = match std::env::var("BT_EVAL_FILTER_PARSED").ok() {
        Some(s) if !s.is_empty() => s,
        _ => return vec![],
    };

    let parsed: Vec<Value> = match serde_json::from_str(&serialized) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Invalid BT_EVAL_FILTER_PARSED: {}", e);
            return vec![];
        }
    };

    parsed
        .into_iter()
        .filter_map(|v| {
            let path = v["path"]
                .as_array()?
                .iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect();
            let pattern_str = v["pattern"].as_str()?;
            let pattern = regex::Regex::new(pattern_str)
                .map_err(|e| eprintln!("Invalid filter pattern '{}': {}", pattern_str, e))
                .ok()?;
            Some(EvalFilter { path, pattern })
        })
        .collect()
}

/// SSE event writer that sends events over a socket to the `bt eval` CLI.
///
/// Events are formatted as Server-Sent Events:
/// ```text
/// event: <name>
/// data: <json>
///
/// ```
struct SseWriter {
    writer: Mutex<Box<dyn Write + Send>>,
}

impl SseWriter {
    fn send(&self, event: &str, data: &Value) {
        let data_str = serde_json::to_string(data).unwrap_or_default();
        if let Ok(mut w) = self.writer.lock() {
            let _ = write!(w, "event: {}\ndata: {}\n\n", event, data_str);
            let _ = w.flush();
        }
    }
}

fn create_sse_writer() -> Option<Arc<SseWriter>> {
    // Unix socket SSE is only available on Unix platforms; Windows continues
    // to the TCP fallback below.
    #[cfg(unix)]
    if let Ok(sock_path) = std::env::var("BT_EVAL_SSE_SOCK") {
        if !sock_path.is_empty() {
            match std::os::unix::net::UnixStream::connect(&sock_path) {
                Ok(stream) => {
                    return Some(Arc::new(SseWriter {
                        writer: Mutex::new(Box::new(stream)),
                    }));
                }
                Err(e) => {
                    eprintln!("Failed to connect to SSE socket {}: {}", sock_path, e);
                }
            }
        }
    }

    // Try TCP address (BT_EVAL_SSE_ADDR)
    if let Ok(addr) = std::env::var("BT_EVAL_SSE_ADDR") {
        if !addr.is_empty() {
            match std::net::TcpStream::connect(&addr) {
                Ok(stream) => {
                    let _ = stream.set_nodelay(true);
                    return Some(Arc::new(SseWriter {
                        writer: Mutex::new(Box::new(stream)),
                    }));
                }
                Err(e) => {
                    eprintln!("Failed to connect to SSE address {}: {}", addr, e);
                }
            }
        }
    }

    None
}

/// A runner designed to work with the `bt eval` CLI.
///
/// This runner reads configuration from environment variables set by the `bt eval` CLI
/// and handles SSE output, list mode, filtering, and other CLI-driven behaviors.
///
/// # Environment Variables
///
/// | Variable | Description |
/// |---|---|
/// | `BT_EVAL_LIST` | List evaluator names without running them |
/// | `BT_EVAL_JSONL` | Output summaries as JSONL to stdout |
/// | `BT_EVAL_SSE_SOCK` | Unix socket path for SSE output |
/// | `BT_EVAL_SSE_ADDR` | `host:port` for SSE output |
/// | `BT_EVAL_FILTER_PARSED` | JSON array of `{path, pattern}` filters |
/// | `BT_EVAL_TERMINATE_ON_FAILURE` | Stop after first failure |
/// | `BT_EVAL_NO_SEND_LOGS` | Skip sending logs to Braintrust |
/// | `BT_EVAL_LOCAL` | Alias for `BT_EVAL_NO_SEND_LOGS` |
///
/// # Example
///
/// ```no_run
/// use braintrust_sdk_rust::eval::*;
///
/// # async fn example() -> braintrust_sdk_rust::Result<()> {
/// let mut runner = BtEvalRunner::from_env().await?;
///
/// runner.eval(
///     "my-project",
///     EvalOpts::builder()
///         .name("my-eval".to_string())
///         .dataset(vec![Case {
///             input: "hello".to_string(),
///             expected: Some("HELLO".to_string()),
///             ..Default::default()
///         }].into())
///         .task(Box::new(SimpleFnTask::new(|input: &String| Ok(input.to_uppercase()))))
///         .scorers(vec![Box::new(ExactMatch::new())])
///         .build(),
/// ).await?;
///
/// let _passed = runner.finish().await?;
/// Ok(())
/// # }
/// ```
pub struct BtEvalRunner {
    config: RunnerConfig,
    sse: Option<Arc<SseWriter>>,
    client: Option<Arc<BraintrustClient>>,
    /// Names registered in list mode, in order of registration.
    registered_names: Vec<String>,
    /// Whether all evals that have run so far passed (no failures).
    all_passed: bool,
}

impl BtEvalRunner {
    /// Create a new runner, reading configuration from environment variables.
    ///
    /// In non-list mode, this creates a `BraintrustClient` from environment variables
    /// (e.g. `BRAINTRUST_API_KEY`). In list mode, no client is created.
    pub async fn from_env() -> Result<Self> {
        let config = RunnerConfig::from_env();
        let sse = create_sse_writer();

        // Only create a client when we'll actually run evals
        let client = if !config.list {
            Some(Arc::new(BraintrustClient::builder().build().await?))
        } else {
            None
        };

        Ok(Self {
            config,
            sse,
            client,
            registered_names: Vec::new(),
            all_passed: true,
        })
    }

    /// Create a new runner with an explicit client.
    ///
    /// Environment variables are still read for mode configuration (list, JSONL, SSE, etc.),
    /// but the provided client is used instead of creating a new one.
    pub fn with_client(client: Arc<BraintrustClient>) -> Self {
        let config = RunnerConfig::from_env();
        let sse = create_sse_writer();
        Self {
            config,
            sse,
            client: Some(client),
            registered_names: Vec::new(),
            all_passed: true,
        }
    }

    /// Returns `true` if the runner is in list mode (`BT_EVAL_LIST=1`).
    pub fn is_list_mode(&self) -> bool {
        self.config.list
    }

    /// Returns `true` if logging to Braintrust is disabled.
    pub fn no_send_logs(&self) -> bool {
        self.config.no_send_logs
    }

    /// Returns `true` if all evals that have run so far passed.
    pub fn all_passed(&self) -> bool {
        self.all_passed
    }

    fn passes_filter(&self, name: &str) -> bool {
        if self.config.filters.is_empty() {
            return true;
        }
        self.config.filters.iter().any(|f| f.matches(name))
    }

    /// Register and/or run an evaluator.
    ///
    /// - In **list mode**: records the eval name and returns without running.
    /// - In **normal mode**: runs the evaluator and sends results via SSE if configured.
    ///
    /// If the eval name does not pass the configured filters, it is skipped silently.
    pub async fn eval<I, O>(&mut self, project: &str, mut opts: EvalOpts<I, O>) -> Result<()>
    where
        I: Serialize + DeserializeOwned + Send + Sync + Clone + 'static,
        O: Serialize + DeserializeOwned + Send + Sync + PartialEq + Clone + 'static,
    {
        let name = opts.name.clone();

        if !self.passes_filter(&name) {
            return Ok(());
        }

        if self.config.list {
            self.registered_names.push(name);
            return Ok(());
        }

        // Set the project name from the argument if not already set in opts
        if opts.project_name.is_none() {
            opts.project_name = Some(project.to_string());
        }

        let client = self
            .client
            .clone()
            .ok_or_else(|| BraintrustError::Background("BtEvalRunner has no client".to_string()))?;

        let evaluator = Evaluator::<I, O>::from_arc(client);

        if let Some(sse) = &self.sse {
            sse.send("start", &json!({ "name": name }));
        }

        match evaluator.run(opts).await {
            Ok(summary) => {
                let failed = summary.failed_cases;

                if let Some(sse) = &self.sse {
                    let summary_value = serde_json::to_value(&summary).unwrap_or(Value::Null);
                    sse.send("summary", &summary_value);
                } else if self.config.jsonl {
                    if let Ok(json) = serde_json::to_string(&summary) {
                        println!("{}", json);
                    }
                }

                if failed > 0 {
                    self.all_passed = false;
                    if self.config.terminate_on_failure {
                        return Err(BraintrustError::Background(format!(
                            "Evaluator '{}' failed with {} error(s)",
                            name, failed
                        )));
                    }
                }
            }
            Err(e) => {
                self.all_passed = false;
                if let Some(sse) = &self.sse {
                    sse.send("error", &json!({ "message": e.to_string() }));
                } else {
                    eprintln!("Evaluator '{}' failed: {}", name, e);
                }
                if self.config.terminate_on_failure {
                    return Err(e);
                }
            }
        }

        Ok(())
    }

    /// Finish the runner session.
    ///
    /// In list mode, prints all registered eval names to stdout (one per line).
    /// The SSE connection (if any) is closed when this method returns.
    ///
    /// Returns `true` if all evals passed, `false` if any failed.
    pub async fn finish(self) -> Result<bool> {
        if self.config.list {
            for name in &self.registered_names {
                println!("{}", name);
            }
        }
        // SSE connection is closed when `self.sse` is dropped
        Ok(self.all_passed)
    }

    /// Convenience method: run multiple evaluations whose results can be summarized.
    ///
    /// Returns `true` if all evals passed.
    pub async fn run_all<F, Fut>(&mut self, evals: Vec<F>) -> Result<bool>
    where
        F: FnOnce(&mut Self) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        for eval_fn in evals {
            eval_fn(self).await?;
        }
        Ok(self.all_passed)
    }
}

/// Serialize an [`EvalSummary`] in a way compatible with the SSE summary event.
///
/// This is used internally by [`BtEvalRunner`] and is public only for advanced use cases.
pub fn summary_to_value<I, O>(summary: &EvalSummary<I, O>) -> Value
where
    I: Serialize,
    O: Serialize,
{
    serde_json::to_value(summary).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    // Serialize tests that mutate BT_EVAL_FILTER_PARSED to avoid race conditions.
    static FILTER_ENV_MUTEX: Mutex<()> = Mutex::new(());

    #[test]
    fn test_env_flag() {
        // Not set → false
        assert!(!env_flag("BT_TEST_NONEXISTENT_FLAG_XYZ"));
    }

    #[test]
    fn test_parse_filters_empty() {
        let _guard = FILTER_ENV_MUTEX.lock().unwrap();
        // When BT_EVAL_FILTER_PARSED is not set, filters should be empty
        std::env::remove_var("BT_EVAL_FILTER_PARSED");
        let filters = parse_filters();
        assert!(filters.is_empty());
    }

    #[test]
    fn test_parse_filters_valid() {
        let _guard = FILTER_ENV_MUTEX.lock().unwrap();
        std::env::set_var(
            "BT_EVAL_FILTER_PARSED",
            r#"[{"path":["name"],"pattern":"foo.*"}]"#,
        );
        let filters = parse_filters();
        assert_eq!(filters.len(), 1);
        assert!(filters[0].matches("foobar"));
        assert!(!filters[0].matches("bar"));
        std::env::remove_var("BT_EVAL_FILTER_PARSED");
    }

    #[test]
    fn test_parse_filters_invalid_json() {
        let _guard = FILTER_ENV_MUTEX.lock().unwrap();
        std::env::set_var("BT_EVAL_FILTER_PARSED", "not-json");
        let filters = parse_filters();
        assert!(filters.is_empty());
        std::env::remove_var("BT_EVAL_FILTER_PARSED");
    }

    #[test]
    fn test_eval_filter_matches_by_name() {
        let filter = EvalFilter {
            path: vec!["name".to_string()],
            pattern: regex::Regex::new("^my-.*").unwrap(),
        };
        assert!(filter.matches("my-eval"));
        assert!(!filter.matches("your-eval"));
    }

    #[test]
    fn test_eval_filter_empty_path_matches_name() {
        let filter = EvalFilter {
            path: vec![],
            pattern: regex::Regex::new("foo").unwrap(),
        };
        assert!(filter.matches("foo-eval"));
        assert!(!filter.matches("bar-eval"));
    }
}
