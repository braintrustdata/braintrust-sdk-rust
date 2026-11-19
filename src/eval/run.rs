//! Immediate submission of local and recorded evaluations.
use super::{
    executor::{EvalExecutor, EvalPlan},
    recorder::BraintrustRecorder,
    EvalConfigError, EvalOpts, EvalRunError, EvalRunFailure, EvalSummary,
};
use crate::{internal::execution::ExecutionBinding, BraintrustClient, Completion};
use serde::Serialize;

/// Submit a local evaluation immediately, without serialization or a client.
/// Dropping the completion does not cancel accepted computation.
///
/// ```
/// use braintrust_sdk_rust::eval::{self, Case, EvalDefinition, EvalOpts, SimpleFnTask};
/// # fn main() -> anyhow::Result<()> {
/// let definition = EvalDefinition::new(
///     Box::new(SimpleFnTask::new(|input: &usize| Ok(input * 2))), vec![],
/// )?;
/// let options = EvalOpts::builder().name("local".into())
///     .definition(definition).source(vec![Case::builder().input(3).build()].into())
///     .quiet(true).build();
/// let completion = eval::run(options);
/// assert!(completion.is_ok());
/// let summary = futures::executor::block_on(async { completion.await })?;
/// assert_eq!(summary.results()[0].output(), Some(&6));
/// # Ok(())
/// # }
/// ```
pub fn run<I: Send + Sync + 'static, O: Send + Sync + 'static>(
    mut options: EvalOpts<I, O>,
) -> Completion<EvalSummary<I, O>, EvalRunError<I, O>> {
    let runtime = options.runtime.take();
    let mut reporter = options.take_reporter();
    let plan = match EvalPlan::new(options) {
        Ok(plan) => plan,
        Err(error) => return Completion::rejected(error.into()),
    };
    let binding = match ExecutionBinding::new(runtime) {
        Ok(binding) => binding,
        Err(source) => {
            return Completion::rejected(EvalRunError::new(EvalRunFailure::Runtime(source)))
        }
    };
    let callbacks = binding.clone();
    binding.response(async move {
        EvalExecutor::run(plan, None, reporter.as_mut(), Some(&callbacks)).await
    })
}

impl BraintrustClient {
    /// Submit a recorded evaluation on this client's fixed runtime.
    /// A run runtime override is rejected before admission or source access.
    pub fn eval<I: Serialize + Send + Sync + 'static, O: Serialize + Send + Sync + 'static>(
        &self,
        mut options: EvalOpts<I, O>,
    ) -> Completion<EvalSummary<I, O>, EvalRunError<I, O>> {
        if options.runtime.is_some() {
            return Completion::rejected(EvalConfigError::RecordedRuntime.into());
        }
        let mut reporter = options.take_reporter();
        let name = options.name.clone();
        let metadata = options.metadata.clone();
        let tags = options.tags.clone();
        let plan = match EvalPlan::new(options) {
            Ok(plan) => plan,
            Err(error) => return Completion::rejected(error.into()),
        };
        let client = self.clone();
        let binding = client.execution_binding();
        let callbacks = binding.clone();
        binding.response_try(|| {
            let operation = client.span_operation()?;
            Ok(async move {
                let recorder = BraintrustRecorder::new(
                    client,
                    operation.submitter.clone(),
                    name,
                    metadata,
                    tags,
                )
                .await?;
                let result =
                    EvalExecutor::run(plan, Some(&recorder), reporter.as_mut(), Some(&callbacks))
                        .await;
                drop(recorder);
                // Revoke write permission before publishing, including escaped spans.
                drop(operation);
                result
            })
        })
    }
}
