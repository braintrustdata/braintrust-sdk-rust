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

use serde::Serialize;
use serde_json::{json, Value};

use crate::{internal::execution::ExecutionBinding, BraintrustClient, BraintrustError, Completion};

use super::{
    cli_session::{ready, Session},
    options::EvalOpts,
    submission::{self, RunReporter},
    types::EvalSummary,
    BtEvalError, BtFinishError, BtInitError, EvalReporter, JsonlReporter, ReportError,
};

mod config;
#[cfg(test)]
use config::{env_flag, parse_filters, EvalFilter};
use config::{RunnerConfig, SseEndpoint};

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
    fn send(&self, event: &str, data: &Value) -> std::result::Result<(), ReportError> {
        let data_str = serde_json::to_string(data)?;
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| std::io::Error::other("SSE writer lock poisoned"))?;
        write!(writer, "event: {}\ndata: {}\n\n", event, data_str)?;
        writer.flush()?;
        Ok(())
    }
}

struct SseReporter {
    sse: Arc<SseWriter>,
}
impl<I: Serialize, O: Serialize> EvalReporter<I, O> for SseReporter {
    fn started(&mut self, name: &str) -> std::result::Result<(), ReportError> {
        self.sse.send("start", &json!({"name": name}))
    }
    fn finished(&mut self, summary: &EvalSummary<I, O>) -> std::result::Result<(), ReportError> {
        self.sse.send("summary", &summary_to_value(summary)?)
    }
}

fn connect_sse(endpoint: SseEndpoint) -> std::result::Result<Arc<SseWriter>, BtInitError> {
    let writer: Box<dyn Write + Send> = match endpoint {
        #[cfg(unix)]
        SseEndpoint::Unix(path) => Box::new(
            std::os::unix::net::UnixStream::connect(path).map_err(|source| BtInitError::Sse {
                transport: "Unix socket",
                source,
            })?,
        ),
        SseEndpoint::Tcp(address) => {
            let stream =
                std::net::TcpStream::connect(address).map_err(|source| BtInitError::Sse {
                    transport: "TCP",
                    source,
                })?;
            let _ = stream.set_nodelay(true);
            Box::new(stream)
        }
    };
    Ok(Arc::new(SseWriter {
        writer: Mutex::new(writer),
    }))
}
async fn prepare_sse(
    binding: &ExecutionBinding,
    endpoint: Option<SseEndpoint>,
) -> std::result::Result<Option<Arc<SseWriter>>, BtInitError> {
    let Some(endpoint) = endpoint else {
        return Ok(None);
    };
    let work = binding.spawn_blocking(move || connect_sse(endpoint));
    struct Abort(tokio::task::AbortHandle);
    impl Drop for Abort {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _abort = Abort(work.abort_handle());
    work.await
        .map_err(|_| BtInitError::Execution(crate::ExecutionError::WorkerLost))?
        .map(Some)
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
/// # async fn example() -> anyhow::Result<()> {
/// let initialization = BtEvalRunner::from_env(); // initialization is admitted now
/// let mut runner = initialization.await?;
///
/// let definition = EvalDefinition::new(
///     Box::new(SimpleFnTask::new(|input: &String| Ok(input.to_uppercase()))),
///     vec![Box::new(ExactMatch::new())],
/// )?;
///
/// let completion = runner.eval(
///     "my-project",
///     EvalOpts::builder()
///         .name("my-eval".to_string())
///         .source(vec![Case {
///             input: "hello".to_string(),
///             expected: Some("HELLO".to_string()),
///             ..Default::default()
///         }].into())
///         .definition(definition.clone())
///         .build(),
/// );
/// assert!(completion.is_ok()); // accepted before awaiting
/// completion.await?;
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
    binding: Option<ExecutionBinding>,
    session: Session,
}

impl BtEvalRunner {
    /// Create a new runner, reading configuration from environment variables.
    ///
    /// Recording mode creates a client from environment variables (e.g.
    /// `BRAINTRUST_API_KEY`). List and local modes create no client. Configuration
    /// is captured and initialization admitted at the call; awaiting only observes
    /// the result. Invalid filters reject immediately; SSE failures are returned
    /// without switching output transports. Socket setup precedes client login.
    pub fn from_env() -> Completion<Self, BtInitError> {
        let config = match RunnerConfig::from_env() {
            Ok(config) => config,
            Err(error) => return Completion::rejected(error),
        };
        Self::initialize(config, None)
    }

    fn assemble(
        config: RunnerConfig,
        sse: Option<Arc<SseWriter>>,
        client: Option<Arc<BraintrustClient>>,
        binding: Option<ExecutionBinding>,
    ) -> Self {
        let session = Session::new(config.terminate_on_failure);
        Self {
            config,
            sse,
            client,
            binding,
            session,
            registered_names: Vec::new(),
        }
    }

    /// Initialize now on this client's fixed binding. Dropping the completion
    /// does not cancel socket setup. List mode uses neither its runtime nor SSE.
    /// Configuration is captured from the environment at the call.
    pub fn with_client(client: Arc<BraintrustClient>) -> Completion<Self, BtInitError> {
        let config = match RunnerConfig::from_env() {
            Ok(config) => config,
            Err(error) => return Completion::rejected(error),
        };
        Self::initialize(config, Some(client))
    }

    fn initialize(
        config: RunnerConfig,
        supplied: Option<Arc<BraintrustClient>>,
    ) -> Completion<Self, BtInitError> {
        if config.list {
            return ready(Self::assemble(config, None, supplied, None));
        }
        if let Some(client) = supplied {
            let binding = client.execution_binding();
            let context = binding.clone();
            return binding.response(async move {
                let sse = prepare_sse(&context, config.endpoint.clone()).await?;
                Ok(Self::assemble(config, sse, Some(client), Some(context)))
            });
        }
        if config.no_send_logs {
            let binding = match ExecutionBinding::new(None) {
                Ok(binding) => binding,
                Err(error) => return Completion::rejected(error.into()),
            };
            let context = binding.clone();
            binding.response(async move {
                let sse = prepare_sse(&context, config.endpoint.clone()).await?;
                Ok(Self::assemble(config, sse, None, Some(context)))
            })
        } else {
            BraintrustClient::builder().build_with(|builder, binding| async move {
                let sse = prepare_sse(&binding, config.endpoint.clone()).await?;
                let client = builder.initialize(binding.clone()).await?;
                Ok(Self::assemble(
                    config,
                    sse,
                    Some(Arc::new(client)),
                    Some(binding),
                ))
            })
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

    /// Returns true if no completed evaluation or admission has failed.
    /// Pending evaluations are included once `finish()` joins the session.
    pub fn all_passed(&self) -> bool {
        self.session.all_passed()
    }

    fn passes_filter(&self, name: &str) -> bool {
        if self.config.filters.is_empty() {
            return true;
        }
        self.config.filters.iter().any(|f| f.matches(name))
    }

    /// Submit a CLI evaluation now, in call order on the session/client binding.
    /// Dropping its completion does not cancel it. Filtered/list entries complete
    /// immediately without evaluation admission. Local runs use an owned session
    /// runtime; an explicit client supplies its binding. Omit `opts.runtime`.
    ///
    /// With terminate-on-failure enabled, failures are returned with their typed
    /// results, and queued/later evaluations are skipped. Otherwise failures mark
    /// `all_passed()` false and the session continues, as with the CLI protocol.
    /// Admission/configuration errors are still reported synchronously on the token.
    pub fn eval<I, O>(
        &mut self,
        project: &str,
        mut opts: EvalOpts<I, O>,
    ) -> Completion<(), BtEvalError<I, O>>
    where
        I: Serialize + Send + Sync + 'static,
        O: Serialize + Send + Sync + 'static,
    {
        if !self.passes_filter(&opts.name) {
            return ready(());
        }
        if self.config.list {
            self.registered_names.push(opts.name);
            return ready(());
        }
        if self.session.stopped() {
            return Completion::rejected(BtEvalError::Stopped);
        }
        if opts.runtime.is_some() {
            self.session.fail();
            return Completion::rejected(BtEvalError::RuntimeOverride);
        }
        if opts.project_name.is_none() {
            opts.project_name = Some(project.to_string());
        }
        let name = opts.name.clone();
        let reporter: RunReporter<I, O> = if let Some(sse) = &self.sse {
            Box::new(SseReporter { sse: sse.clone() })
        } else if self.config.jsonl {
            Box::new(JsonlReporter::new(std::io::stdout()))
        } else if opts.quiet {
            Box::new(super::SilentReporter)
        } else {
            Box::new(super::TerminalReporter::default())
        };
        let session = self.session.clone();
        let sse = self.sse.clone();
        let terminate = self.config.terminate_on_failure;
        let make = || {
            let (previous, ticket) = session.reserve::<I, O>()?;
            let sequence = ticket.sequence();
            let before = async move {
                previous.await;
                if session.should_skip(sequence) {
                    Err(BtEvalError::Stopped)
                } else {
                    Ok(())
                }
            };
            let outcome_session = self.session.clone();
            let after = move |outcome: std::result::Result<
                EvalSummary<I, O>,
                BtEvalError<I, O>,
            >| {
                let failed = !matches!(&outcome, Ok(summary) if summary.failed_cases == 0);
                let result = match outcome {
                    Ok(summary) if summary.failed_cases > 0 && terminate => {
                        Err(BtEvalError::CaseFailures {
                            summary: Box::new(summary),
                        })
                    }
                    Ok(_) => Ok(()),
                    Err(BtEvalError::Stopped) => Err(BtEvalError::Stopped),
                    Err(error) => {
                        let output_failed = matches!(&error, BtEvalError::Run(run) if run.failures.iter().any(|failure| matches!(failure, super::EvalRunFailure::Reporting { .. })));
                        if let Some(sse) = sse.as_ref().filter(|_| !output_failed) {
                            if let Err(output) =
                                sse.send("error", &json!({"message":error.to_string()}))
                            {
                                eprintln!(
                                    "Evaluator '{}' failed: {}; error output also failed: {}",
                                    name, error, output
                                );
                            }
                        } else {
                            eprintln!("Evaluator '{}' failed: {}", name, error);
                        }
                        if terminate {
                            Err(error)
                        } else {
                            Ok(())
                        }
                    }
                };
                if failed {
                    outcome_session.fail_at(sequence);
                }
                result
            };
            Ok((before, after, move |published: bool| {
                ticket.complete(!published)
            }))
        };
        let token = if self.config.no_send_logs {
            submission::local(
                opts,
                self.binding
                    .as_ref()
                    .expect("non-list session has a binding")
                    .clone(),
                reporter,
                make,
            )
        } else if let Some(client) = &self.client {
            submission::recorded(client.as_ref().clone(), opts, reporter, make)
        } else {
            Completion::rejected(BtEvalError::Run(
                BraintrustError::Background("BtEvalRunner has no client".into()).into(),
            ))
        };
        if token.is_err() {
            self.session.fail();
        }
        token
    }

    /// Close the session and join every accepted evaluation, including discarded
    /// observers. Output and recording finish before this completion is published.
    /// The supplied client remains usable; this does not shut down shared clients.
    /// Returns false if any evaluation/admission failed or lost its worker.
    pub fn finish(self) -> Completion<bool, BtFinishError> {
        if self.config.list {
            let mut output = std::io::stdout().lock();
            for name in &self.registered_names {
                if let Err(error) = writeln!(output, "{}", name) {
                    return Completion::rejected(BtFinishError::Output(error.into()));
                }
            }
            if let Err(error) = output.flush() {
                return Completion::rejected(BtFinishError::Output(error.into()));
            }
            return ready(self.session.all_passed());
        }
        let binding = self
            .binding
            .as_ref()
            .expect("non-list session has a binding")
            .clone();
        let (sender, completion) = Completion::pair();
        // One consuming control operation, independent of full run capacity.
        binding.spawn(async move {
            self.session.tail().await;
            let passed = self.session.all_passed();
            drop(self); // Close the CLI socket and release ownership before publication.
            let _ = sender.send(Ok(passed));
        });
        completion
    }
}

/// Serialize an [`EvalSummary`] in a way compatible with the SSE summary event.
///
/// Serialization failures are returned with their cause; no null placeholder is emitted.
pub fn summary_to_value<I, O>(summary: &EvalSummary<I, O>) -> serde_json::Result<Value>
where
    I: Serialize,
    O: Serialize,
{
    serde_json::to_value(summary)
}

#[cfg(test)]
mod tests;
