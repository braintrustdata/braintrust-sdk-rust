//! Immediate submission of local and recorded evaluations.
use super::{executor::EvalPlan, submission, EvalOpts, EvalRunError, EvalRunFailure, EvalSummary};
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
/// let summary = completion.blocking_wait()?;
/// assert_eq!(summary.results()[0].output(), Some(&6));
/// # Ok(())
/// # }
/// ```
pub fn run<I: Send + Sync + 'static, O: Send + Sync + 'static>(
    mut options: EvalOpts<I, O>,
) -> Completion<EvalSummary<I, O>, EvalRunError<I, O>> {
    let runtime = options.runtime.take();
    let reporter = options.take_reporter();
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
    submission::local(plan, binding, reporter, || {
        Ok((std::future::ready(Ok(())), std::convert::identity, |_| {}))
    })
}

impl BraintrustClient {
    /// Submit a recorded evaluation on this client's fixed runtime.
    /// A run runtime override is rejected before admission or source access.
    pub fn eval<I: Serialize + Send + Sync + 'static, O: Serialize + Send + Sync + 'static>(
        &self,
        mut options: EvalOpts<I, O>,
    ) -> Completion<EvalSummary<I, O>, EvalRunError<I, O>> {
        let reporter = options.take_reporter();
        submission::recorded(self.clone(), options, reporter, || {
            Ok((std::future::ready(Ok(())), std::convert::identity, |_| {}))
        })
    }
}
