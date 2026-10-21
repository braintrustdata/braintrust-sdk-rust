//! One response reservation per run, shared by public and CLI entry points.
use super::{
    executor::{EvalExecutor, EvalPlan},
    recorder::BraintrustRecorder,
    EvalConfigError, EvalOpts, EvalReporter, EvalRunError, EvalSummary,
};
use crate::{
    internal::execution::ExecutionBinding, BraintrustClient, Completion, EnqueueError,
    ExecutionError,
};
use serde::Serialize;
use std::future::Future;

pub(crate) type RunReporter<I, O> = Box<dyn EvalReporter<I, O>>;

// Hooks are made only after response/write admission. They provide CLI ordering
// and result translation without a second response task or runtime.
pub(crate) fn local<I, O, T, E, M, B, A, C>(
    options: EvalOpts<I, O>,
    binding: ExecutionBinding,
    mut reporter: RunReporter<I, O>,
    make: M,
) -> Completion<T, E>
where
    I: Send + Sync + 'static,
    O: Send + Sync + 'static,
    T: Send + 'static,
    E: From<EvalRunError<I, O>> + From<ExecutionError> + From<EnqueueError> + Send + 'static,
    M: FnOnce() -> Result<(B, A, C), E>,
    B: Future<Output = Result<(), E>> + Send + 'static,
    A: FnOnce(Result<EvalSummary<I, O>, E>) -> Result<T, E> + Send + 'static,
    C: FnOnce(bool) + Send + 'static,
{
    if let Err(error) = options.validate() {
        return Completion::rejected(E::from(EvalRunError::from(error)));
    }
    let callbacks = binding.clone();
    binding.response_try_finalized(|| {
        let (before, after, finalized) = make()?;
        Ok((
            async move {
                let result = match before.await {
                    Ok(()) => {
                        let plan = EvalPlan::new(options)
                            .map_err(EvalRunError::from)
                            .map_err(E::from)?;
                        EvalExecutor::run(plan, None, reporter.as_mut(), Some(&callbacks))
                            .await
                            .map_err(E::from)
                    }
                    Err(error) => Err(error),
                };
                drop(reporter);
                after(result)
            },
            finalized,
        ))
    })
}

pub(crate) fn recorded<I, O, T, E, M, B, A, C>(
    client: BraintrustClient,
    options: EvalOpts<I, O>,
    mut reporter: RunReporter<I, O>,
    make: M,
) -> Completion<T, E>
where
    I: Serialize + Send + Sync + 'static,
    O: Serialize + Send + Sync + 'static,
    T: Send + 'static,
    E: From<EvalRunError<I, O>> + From<ExecutionError> + From<EnqueueError> + Send + 'static,
    M: FnOnce() -> Result<(B, A, C), E>,
    B: Future<Output = Result<(), E>> + Send + 'static,
    A: FnOnce(Result<EvalSummary<I, O>, E>) -> Result<T, E> + Send + 'static,
    C: FnOnce(bool) + Send + 'static,
{
    if let Err(error) = options.validate() {
        return Completion::rejected(E::from(EvalRunError::from(error)));
    }
    if options.runtime.is_some() {
        return Completion::rejected(E::from(EvalRunError::from(
            EvalConfigError::RecordedRuntime,
        )));
    }
    let binding = client.execution_binding();
    let callbacks = binding.clone();
    binding.response_try_finalized(|| {
        let operation = client
            .span_operation()
            .map_err(EvalRunError::from)
            .map_err(E::from)?;
        let (before, after, finalized) = make()?;
        Ok((
            async move {
                let result = match before.await {
                    Ok(()) => {
                        let mut recorder = BraintrustRecorder::new(
                            client,
                            operation.submitter.clone(),
                            options.project_name.clone(),
                            options
                                .experiment_name
                                .clone()
                                .unwrap_or_else(|| options.name.clone()),
                            options.metadata.clone(),
                        );
                        let plan = EvalPlan::new(options)
                            .map_err(EvalRunError::from)
                            .map_err(E::from)?;
                        EvalExecutor::run(
                            plan,
                            Some(&mut recorder),
                            reporter.as_mut(),
                            Some(&callbacks),
                        )
                        .await
                        .map_err(E::from)
                    }
                    Err(error) => Err(error),
                };
                drop(reporter);
                // Revoke even escaped spans before status, ordering or token publication.
                drop(operation);
                after(result)
            },
            finalized,
        ))
    })
}
