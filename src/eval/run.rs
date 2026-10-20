//! Immediate submission of local and recorded evaluations.
use super::{
    executor::{EvalExecutor, EvalPlan},
    recorder::BraintrustRecorder,
    reporter::TerminalReporter,
    EvalConfigError, EvalOpts, EvalRunError, EvalRunFailure, EvalSummary,
};
use crate::{internal::execution::ExecutionBinding, BraintrustClient, Completion};
use serde::Serialize;

type RunCompletion<I, O> = Completion<EvalSummary<I, O>, EvalRunError<I, O>>;
type SubmitRecorded<I, O> = fn(BraintrustClient, EvalOpts<I, O>) -> RunCompletion<I, O>;

/// An owned invocation of a reusable definition with its own source and options.
///
/// Local requests need no client or serialization. Recorded requests require
/// serialization and inherit their client's execution binding and lifecycle.
pub struct EvalRequest<I, O> {
    options: EvalOpts<I, O>,
    target: Target<I, O>,
}
enum Target<I, O> {
    Local,
    Recorded {
        client: BraintrustClient,
        submit: SubmitRecorded<I, O>,
    },
}
impl<I, O> EvalRequest<I, O> {
    /// Run without login, HTTP or recording, on `options.runtime` or an owned runtime.
    pub fn local(options: EvalOpts<I, O>) -> Self {
        Self {
            options,
            target: Target::Local,
        }
    }
}
impl<I: Serialize + Send + Sync + 'static, O: Serialize + Send + Sync + 'static> EvalRequest<I, O> {
    /// Record with this client's runtime. Supplying `options.runtime` is an error.
    pub fn recorded(options: EvalOpts<I, O>, client: BraintrustClient) -> Self {
        Self {
            options,
            target: Target::Recorded {
                client,
                submit: recorded::<I, O>,
            },
        }
    }
}

/// Submit an evaluation now; awaiting observes the admitted run's result.
/// Dropping the token does not cancel accepted computation or recording.
///
/// ```
/// use braintrust_sdk_rust::eval::{self, Case, EvalDefinition, EvalOpts, EvalRequest, SimpleFnTask};
/// # fn main() -> anyhow::Result<()> {
/// let definition = EvalDefinition::new(
///     Box::new(SimpleFnTask::new(|input: &usize| Ok(input * 2))), vec![],
/// )?;
/// let options = EvalOpts::builder().name("local".into())
///     .definition(definition).source(vec![Case { input: 3, ..Default::default() }].into())
///     .quiet(true).build();
/// let completion = eval::run(EvalRequest::local(options));
/// assert!(completion.is_ok()); // admission already happened
/// let summary = futures::executor::block_on(async { completion.await })?;
/// assert_eq!(summary.results[0].output, Some(6));
/// # Ok(())
/// # }
/// ```
pub fn run<I: Send + Sync + 'static, O: Send + Sync + 'static>(
    request: EvalRequest<I, O>,
) -> RunCompletion<I, O> {
    if let Err(error) = request.options.validate() {
        return Completion::rejected(error.into());
    }
    match request.target {
        Target::Local => {
            let mut options = request.options;
            let binding = match ExecutionBinding::new(options.runtime.take()) {
                Ok(binding) => binding,
                Err(source) => {
                    return Completion::rejected(EvalRunError::new(EvalRunFailure::Runtime(source)))
                }
            };
            let callbacks = binding.clone();
            binding.response(async move {
                let mut reporter = TerminalReporter {
                    quiet: options.quiet,
                };
                EvalExecutor::run(
                    EvalPlan::new(options)?,
                    None,
                    &mut reporter,
                    Some(&callbacks),
                )
                .await
            })
        }
        Target::Recorded { client, submit } => {
            if request.options.runtime.is_some() {
                return Completion::rejected(EvalConfigError::RecordedRuntime.into());
            }
            submit(client, request.options)
        }
    }
}

fn recorded<I: Serialize + Send + Sync + 'static, O: Serialize + Send + Sync + 'static>(
    client: BraintrustClient,
    options: EvalOpts<I, O>,
) -> RunCompletion<I, O> {
    let binding = client.execution_binding();
    let callbacks = binding.clone();
    binding.response_try(|| {
        let operation = client.span_operation()?;
        Ok(async move {
            let mut reporter = TerminalReporter {
                quiet: options.quiet,
            };
            let recorder = BraintrustRecorder::new(
                client,
                operation.submitter.clone(),
                options.project_name.clone(),
                options
                    .experiment_name
                    .clone()
                    .unwrap_or_else(|| options.name.clone()),
                options.metadata.clone(),
            )
            .await?;
            let result = EvalExecutor::run(
                EvalPlan::new(options)?,
                Some(&recorder),
                &mut reporter,
                Some(&callbacks),
            )
            .await;
            drop(recorder);
            // Revoke write permission before publishing, including escaped spans.
            drop(operation);
            result
        })
    })
}
