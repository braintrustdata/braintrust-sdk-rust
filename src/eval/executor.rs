//! Bounded run scheduling and aggregation, independent of recording and output.
use async_trait::async_trait;
use futures::{
    future::{select, Either},
    stream::{FuturesUnordered, StreamExt},
};

use super::{
    case_runner::CaseRunner, Case, EvalDefinition, EvalOpts, EvalResult, EvalRunError,
    EvalRunFailure, EvalSummary,
};
use crate::{BraintrustError, Result, SpanHandle};

/// An invocation owns its source and options; only the definition is shared.
pub(crate) struct EvalPlan<I, O> {
    name: String,
    source: Box<dyn super::CaseSource<I, O>>,
    definition: EvalDefinition<I, O>,
    concurrency: usize,
    metadata: Option<serde_json::Map<String, serde_json::Value>>,
    tags: Option<Vec<String>>,
}
impl<I, O> EvalPlan<I, O> {
    pub fn new(opts: EvalOpts<I, O>) -> std::result::Result<Self, super::EvalConfigError> {
        opts.validate()?;
        Ok(Self {
            name: opts.name,
            source: opts.source,
            definition: opts.definition,
            concurrency: opts.max_concurrency,
            metadata: opts.metadata,
            tags: opts.tags,
        })
    }
    fn apply_context(&self, case: &mut Case<I, O>) {
        if let Some(defaults) = &self.metadata {
            let mut metadata = defaults.clone();
            if let Some(specific) = case.metadata.take() {
                metadata.extend(specific);
            }
            case.metadata = Some(metadata);
        }
        if let Some(defaults) = &self.tags {
            let mut tags = Vec::new();
            for tag in defaults.iter().chain(case.tags.iter().flatten()) {
                if !tags.contains(tag) {
                    tags.push(tag.clone());
                }
            }
            case.tags = Some(tags);
        }
    }
}

#[async_trait]
pub(crate) trait EvalRecorder<I, O>: Send + Sync {
    fn target(&self) -> Option<super::EvalRecording> {
        None
    }
    async fn start(&self, index: usize, case: &Case<I, O>) -> Result<Box<dyn CaseRecording<I, O>>>;
    async fn finish(&self) -> Vec<BraintrustError>;
}

#[async_trait]
pub(crate) trait CaseRecording<I, O>: Send {
    fn span(&self) -> Option<&SpanHandle>;
    async fn finish(self: Box<Self>, result: &mut EvalResult<I, O>) -> Vec<BraintrustError>;
}

pub(crate) trait EvalReporter<I, O> {
    fn started(&mut self, name: &str);
    fn case_finished(&mut self, index: usize, result: &EvalResult<I, O>);
    fn finished(&mut self, summary: &EvalSummary<I, O>);
}

struct CaseOutcome<I, O> {
    result: EvalResult<I, O>,
    failures: Vec<BraintrustError>,
}

pub(crate) struct EvalExecutor;
impl EvalExecutor {
    pub async fn run<I: Send + Sync + 'static, O: Send + Sync + 'static>(
        mut plan: EvalPlan<I, O>,
        recorder: Option<&dyn EvalRecorder<I, O>>,
        reporter: &mut impl EvalReporter<I, O>,
        binding: Option<&crate::internal::execution::ExecutionBinding>,
    ) -> std::result::Result<EvalSummary<I, O>, EvalRunError<I, O>> {
        reporter.started(&plan.name);
        let prepared = plan.source.prepare().await;
        let dataset = plan.source.provenance();
        let mut results = Vec::new();
        let mut failures = Vec::new();
        if let Err(source) = prepared {
            failures.push(EvalRunFailure::Source {
                case_index: 0,
                source,
            });
        }
        let mut case_count = 0;
        let mut pending = FuturesUnordered::new();
        while failures.is_empty() {
            // Keep one source read alive while cases run. Sources may await
            // case progress and need not support cancellation/restarting next().
            let next_case = {
                let mut next = Box::pin(plan.source.next());
                loop {
                    if pending.is_empty() {
                        break next.await;
                    }
                    match select(next.as_mut(), pending.next()).await {
                        Either::Left((case, _)) => break case,
                        Either::Right((Some((index, outcome)), _)) => {
                            Self::collect(index, outcome, &mut results, &mut failures, reporter);
                            if !failures.is_empty() {
                                break Ok(None);
                            }
                        }
                        Either::Right((None, _)) => unreachable!("nonempty pending cases"),
                    }
                }
            };
            let mut case = match next_case {
                Ok(Some(case)) => case,
                Ok(None) => break,
                Err(source) => {
                    failures.push(EvalRunFailure::Source {
                        case_index: case_count,
                        source,
                    });
                    break;
                }
            };
            plan.apply_context(&mut case);
            let index = case_count;
            case_count += 1;
            let definition = &plan.definition;
            pending.push(async move {
                (
                    index,
                    Self::run_case(case, definition, recorder, index, binding).await,
                )
            });
            if pending.len() >= plan.concurrency {
                if let Some((index, outcome)) = pending.next().await {
                    Self::collect(index, outcome, &mut results, &mut failures, reporter);
                }
                if !failures.is_empty() {
                    break;
                }
            }
        }
        // Stop reading after failure, but finish all cases already accepted.
        while let Some((index, outcome)) = pending.next().await {
            Self::collect(index, outcome, &mut results, &mut failures, reporter);
        }
        if let Some(recorder) = recorder {
            failures.extend(recorder.finish().await.into_iter().map(|source| {
                EvalRunFailure::Recording {
                    case_index: None,
                    source,
                }
            }));
        }
        results.sort_unstable_by_key(|(index, _)| *index);
        let results: Vec<_> = results.into_iter().map(|(_, result)| result).collect();
        let score_stats = EvalSummary::<I, O>::compute_stats(&results);
        let successful_cases = results
            .iter()
            .filter(|result| result.errors.is_empty())
            .count();
        let summary = EvalSummary {
            name: plan.name,
            results,
            score_stats,
            total_cases: case_count,
            successful_cases,
            failed_cases: case_count - successful_cases,
            dataset,
            recording: recorder.and_then(|recorder| recorder.target()),
        };
        reporter.finished(&summary);
        if failures.is_empty() {
            Ok(summary)
        } else {
            Err(EvalRunError {
                failures,
                partial: Some(Box::new(summary)),
            })
        }
    }

    async fn run_case<I: Send + Sync + 'static, O: Send + Sync + 'static>(
        case: Case<I, O>,
        definition: &EvalDefinition<I, O>,
        recorder: Option<&dyn EvalRecorder<I, O>>,
        index: usize,
        binding: Option<&crate::internal::execution::ExecutionBinding>,
    ) -> CaseOutcome<I, O> {
        let mut failures = Vec::new();
        let recording = if let Some(recorder) = recorder {
            match recorder.start(index, &case).await {
                Ok(recording) => Some(recording),
                Err(error) => {
                    failures.push(error);
                    None
                }
            }
        } else {
            None
        };
        let span = recording.as_ref().and_then(|recording| recording.span());
        let mut result = CaseRunner::run(case, definition, span, binding).await;
        if let Some(recording) = recording {
            failures.extend(recording.finish(&mut result).await);
        }
        CaseOutcome { result, failures }
    }

    fn collect<I, O>(
        index: usize,
        outcome: CaseOutcome<I, O>,
        results: &mut Vec<(usize, EvalResult<I, O>)>,
        failures: &mut Vec<EvalRunFailure>,
        reporter: &mut impl EvalReporter<I, O>,
    ) {
        failures.extend(
            outcome
                .failures
                .into_iter()
                .map(|source| EvalRunFailure::Recording {
                    case_index: Some(index),
                    source,
                }),
        );
        reporter.case_finished(index, &outcome.result);
        results.push((index, outcome.result));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{task, CaseSource, SourceError};
    use std::{
        cell::Cell,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Mutex,
        },
    };

    struct Input(usize);
    struct Output(usize);
    fn case(input: usize) -> Case<Input, Output> {
        Case {
            input: Input(input),
            expected: None,
            metadata: None,
            tags: None,
            id: Some(input.to_string()),
            xact_id: None,
            created: None,
        }
    }
    #[derive(Default)]
    struct Observed {
        cases: Vec<usize>,
        finished: bool,
    }
    impl<I, O> EvalReporter<I, O> for Observed {
        fn started(&mut self, _: &str) {}
        fn case_finished(&mut self, index: usize, _: &EvalResult<I, O>) {
            self.cases.push(index);
        }
        fn finished(&mut self, _: &EvalSummary<I, O>) {
            self.finished = true;
        }
    }
    struct WaitingSource {
        cursor: Cell<usize>,
        task_progress: Option<futures::channel::oneshot::Receiver<()>>,
        reads: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl CaseSource<Input, Output> for WaitingSource {
        async fn next(&mut self) -> std::result::Result<Option<Case<Input, Output>>, SourceError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            let index = self.cursor.get();
            self.cursor.set(index + 1);
            match index {
                0 => Ok(Some(case(1))),
                1 => {
                    self.task_progress.take().unwrap().await.unwrap();
                    Ok(Some(case(2)))
                }
                _ => anyhow::bail!("source stopped"),
            }
        }
    }

    #[test]
    fn local_run_drives_pending_source_and_drains_cases_without_runtime_or_serialization() {
        let (progress, receiver) = futures::channel::oneshot::channel();
        let progress = Mutex::new(Some(progress));
        let reads = Arc::new(AtomicUsize::new(0));
        let definition = EvalDefinition::new(
            Box::new(task(move |input: &Input, _hooks| {
                if let Some(progress) = progress.lock().unwrap().take() {
                    progress.send(()).unwrap();
                }
                Box::pin(async move { Ok(Output(input.0 * 2)) })
            })),
            vec![],
        )
        .unwrap();
        let plan = EvalPlan::new(
            EvalOpts::builder()
                .name("local".into())
                .source(Box::new(WaitingSource {
                    cursor: Cell::new(0),
                    task_progress: Some(receiver),
                    reads: reads.clone(),
                }))
                .definition(definition)
                .max_concurrency(2)
                .build(),
        )
        .unwrap();
        let mut reporter = Observed::default();
        let error = futures::executor::block_on(EvalExecutor::run(plan, None, &mut reporter, None))
            .err()
            .expect("source should fail");
        assert!(
            matches!(&error.failures[..], [EvalRunFailure::Source {case_index: 2, source}] if source.to_string() == "source stopped")
        );
        let summary = error.partial.unwrap();
        assert_eq!(
            reads.load(Ordering::SeqCst),
            3,
            "pending next() must not restart"
        );
        assert_eq!(summary.total_cases, 2);
        assert_eq!(summary.successful_cases, 2);
        assert_eq!(
            summary
                .results
                .iter()
                .map(|result| result.output.as_ref().unwrap().0)
                .collect::<Vec<_>>(),
            [2, 4]
        );
        assert!(summary
            .results
            .iter()
            .all(|result| result.span_id.is_none()));
        assert_eq!(reporter.cases, [0, 1]);
        assert!(reporter.finished);
    }

    #[test]
    fn local_success_has_bounded_concurrency_and_source_order_without_data_trait_bounds() {
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let definition = EvalDefinition::new(
            Box::new(task({
                let active = active.clone();
                let peak = peak.clone();
                move |input: &Input, _hooks| {
                    let active = active.clone();
                    let peak = peak.clone();
                    let barrier = barrier.clone();
                    Box::pin(async move {
                        let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(count, Ordering::SeqCst);
                        barrier.wait().await;
                        active.fetch_sub(1, Ordering::SeqCst);
                        Ok(Output(input.0 * 2))
                    })
                }
            })),
            vec![],
        )
        .unwrap();
        for count in [4, 0] {
            let plan = EvalPlan::new(
                EvalOpts::builder()
                    .name("local-success".into())
                    .source((0..count).map(case).collect::<Vec<_>>().into())
                    .definition(definition.clone())
                    .max_concurrency(2)
                    .build(),
            )
            .unwrap();
            let mut reporter = Observed::default();
            let summary =
                futures::executor::block_on(EvalExecutor::run(plan, None, &mut reporter, None))
                    .expect("local computation should succeed");
            assert_eq!(summary.total_cases, count);
            assert_eq!(summary.successful_cases, count);
            assert_eq!(
                summary
                    .results
                    .iter()
                    .map(|result| result.output.as_ref().unwrap().0)
                    .collect::<Vec<_>>(),
                (0..count).map(|index| index * 2).collect::<Vec<_>>()
            );
            assert!(reporter.finished);
        }
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert_eq!(peak.load(Ordering::SeqCst), 2);
    }

    struct FailingRecorder {
        finished: Arc<AtomicUsize>,
    }
    struct FailingCase;
    #[async_trait]
    impl EvalRecorder<usize, usize> for FailingRecorder {
        async fn start(
            &self,
            index: usize,
            _: &Case<usize, usize>,
        ) -> Result<Box<dyn CaseRecording<usize, usize>>> {
            if index == 0 {
                return Err(BraintrustError::Background("start failed".into()));
            }
            Ok(Box::new(FailingCase))
        }
        async fn finish(&self) -> Vec<BraintrustError> {
            self.finished.fetch_add(1, Ordering::SeqCst);
            vec![BraintrustError::Background("run finish failed".into())]
        }
    }
    #[async_trait]
    impl CaseRecording<usize, usize> for FailingCase {
        fn span(&self) -> Option<&SpanHandle> {
            None
        }
        async fn finish(self: Box<Self>, _: &mut EvalResult<usize, usize>) -> Vec<BraintrustError> {
            vec![BraintrustError::Background("case finish failed".into())]
        }
    }
    #[test]
    fn recorder_start_and_finish_errors_preserve_computation_and_finalize_run() {
        let finished = Arc::new(AtomicUsize::new(0));
        let recorder = FailingRecorder {
            finished: finished.clone(),
        };
        let cases = (0..3)
            .map(|input| Case {
                input,
                ..Default::default()
            })
            .collect::<Vec<Case<usize, usize>>>();
        let plan = EvalPlan::new(
            EvalOpts::builder()
                .name("recorded".into())
                .source(cases.into())
                .definition(
                    EvalDefinition::new(
                        Box::new(task(|input: &usize, _hooks| {
                            Box::pin(async move { Ok(input + 10) })
                        })),
                        vec![],
                    )
                    .unwrap(),
                )
                .max_concurrency(2)
                .build(),
        )
        .unwrap();
        let mut reporter = Observed::default();
        let error = futures::executor::block_on(EvalExecutor::run(
            plan,
            Some(&recorder),
            &mut reporter,
            None,
        ))
        .unwrap_err();
        assert_eq!(error.failures.len(), 3);
        assert!(matches!(
            &error.failures[0],
            EvalRunFailure::Recording {
                case_index: Some(0),
                ..
            }
        ));
        assert!(matches!(
            &error.failures[1],
            EvalRunFailure::Recording {
                case_index: Some(1),
                ..
            }
        ));
        assert!(matches!(
            &error.failures[2],
            EvalRunFailure::Recording {
                case_index: None,
                ..
            }
        ));
        let summary = error.partial.unwrap();
        assert_eq!(
            summary
                .results
                .iter()
                .map(|result| result.output)
                .collect::<Vec<_>>(),
            [Some(10), Some(11)]
        );
        assert_eq!(summary.total_cases, 2, "third case must remain unread");
        assert_eq!(summary.successful_cases, 2);
        assert_eq!(summary.failed_cases, 0);
        assert_eq!(finished.load(Ordering::SeqCst), 1);
        assert!(reporter.finished);
    }
}
