//! One case's computation, independent of recording and reporting.
use super::{Case, CaseError, EvalDefinition, EvalResult, ScorerArgs, TaskHooks};
use crate::{internal::execution::ExecutionBinding, SpanHandle};
use futures::{stream::FuturesOrdered, StreamExt};
use std::sync::Arc;

pub(crate) struct CaseRunner;
impl CaseRunner {
    pub async fn run<I: Send + Sync + 'static, O: Send + Sync + 'static>(
        case: Case<I, O>,
        definition: &EvalDefinition<I, O>,
        span: Option<&SpanHandle>,
        binding: Option<&ExecutionBinding>,
    ) -> EvalResult<I, O> {
        let case = Arc::new(case);
        let hooks = TaskHooks::for_case(
            span,
            case.expected.as_ref(),
            case.metadata.clone(),
            case.tags.clone(),
        );
        let task = definition.task();
        let computed = if task.is_blocking() {
            let case = case.clone();
            let state = hooks.rebind(None, None);
            let span = span.cloned();
            super::blocking::invoke(binding, move || {
                let hooks = state.rebind(span.as_ref(), case.expected.as_ref());
                task.run_blocking(&case.input, &hooks)
            })
            .await
        } else {
            task.run(&case.input, &hooks).await
        };
        let mut errors = Vec::new();
        let output = match computed {
            Ok(output) => Some(Arc::new(output)),
            Err(source) => {
                errors.push(CaseError::Task { source });
                None
            }
        };
        let mut scores = Vec::new();
        if let Some(output) = &output {
            let mut pending = FuturesOrdered::new();
            for scorer in definition.scorers() {
                let case = case.clone();
                let output = output.clone();
                let scorer = scorer.clone();
                pending.push_back(async move {
                    let name = scorer.name().to_owned();
                    let result = if scorer.is_blocking() {
                        super::blocking::invoke(binding, move || {
                            scorer.score_blocking(ScorerArgs {
                                input: &case.input,
                                output: &output,
                                expected: case.expected.as_ref(),
                                metadata: case.metadata.as_ref(),
                            })
                        })
                        .await
                    } else {
                        scorer
                            .score(ScorerArgs {
                                input: &case.input,
                                output: &output,
                                expected: case.expected.as_ref(),
                                metadata: case.metadata.as_ref(),
                            })
                            .await
                    };
                    result.map_err(|source| CaseError::Scorer { name, source })
                });
            }
            while let Some(result) = pending.next().await {
                match result {
                    Ok(values) => scores.extend(values),
                    Err(error) => errors.push(error),
                }
            }
        }
        let metadata = hooks.get_metadata();
        let tags = hooks.get_tags();
        drop(hooks);
        // All owned invocations have joined. Borrowed callback arguments cannot
        // escape, so no input/output clones are required to return ownership.
        let case =
            Arc::try_unwrap(case).unwrap_or_else(|_| unreachable!("case invocations joined"));
        let output = output.map(|output| {
            Arc::try_unwrap(output).unwrap_or_else(|_| unreachable!("scorers joined"))
        });
        EvalResult {
            input: case.input,
            output,
            expected: case.expected,
            scores,
            metadata,
            tags,
            errors,
            case_id: case.id,
            xact_id: case.xact_id,
            created: case.created,
            span_id: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{Score, Scorer, Scores, Task};
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    #[derive(Debug, thiserror::Error)]
    #[error("application failure")]
    struct ApplicationFailure;

    // These intentionally implement neither Clone, serialization nor equality.
    struct Input(usize);
    struct Output(usize);
    struct LocalTask;
    #[async_trait]
    impl Task<Input, Output> for LocalTask {
        async fn run(
            &self,
            input: &Input,
            hooks: &TaskHooks<'_, Input, Output>,
        ) -> anyhow::Result<Output> {
            assert!(hooks.span.is_none());
            assert_eq!(hooks.expected.unwrap().0, 6);
            hooks.add_metadata("task", json!(true));
            hooks.add_tag("task");
            hooks.add_tag("source");
            Ok(Output(input.0 * 2))
        }
    }

    #[test]
    fn local_computation_owns_nonserializable_values_and_preserves_case_context() {
        let case = Case {
            input: Input(3),
            expected: Some(Output(6)),
            metadata: Some(serde_json::from_value(json!({"source":true})).unwrap()),
            tags: Some(vec!["source".into()]),
            id: Some("case".into()),
            xact_id: Some("version".into()),
            created: Some("timestamp".into()),
        };
        let result = futures::executor::block_on(CaseRunner::run(
            case,
            &EvalDefinition::new(Box::new(LocalTask), vec![]).unwrap(),
            None,
            None,
        ));
        assert_eq!(result.output.unwrap().0, 6);
        assert_eq!(result.case_id.as_deref(), Some("case"));
        assert_eq!(result.xact_id.as_deref(), Some("version"));
        assert_eq!(result.created.as_deref(), Some("timestamp"));
        assert_eq!(
            result.metadata.unwrap(),
            serde_json::from_value::<serde_json::Map<String, serde_json::Value>>(
                json!({"source":true,"task":true})
            )
            .unwrap()
        );
        assert_eq!(result.tags.unwrap(), vec!["source", "task"]);
        assert!(result.errors.is_empty());
        assert!(result.span_id.is_none());
    }

    struct FailTask;
    #[async_trait]
    impl Task<usize, usize> for FailTask {
        async fn run(
            &self,
            _: &usize,
            hooks: &TaskHooks<'_, usize, usize>,
        ) -> anyhow::Result<usize> {
            hooks.add_metadata("failed", json!(true));
            hooks.add_tag("failed");
            Err(ApplicationFailure.into())
        }
    }
    struct ProbeScorer(Arc<AtomicBool>);
    #[async_trait]
    impl Scorer<usize, usize> for ProbeScorer {
        fn name(&self) -> &str {
            "probe"
        }
        async fn score(&self, _: ScorerArgs<'_, usize, usize>) -> anyhow::Result<Scores> {
            self.0.store(true, Ordering::SeqCst);
            Ok(vec![])
        }
    }
    #[tokio::test]
    async fn task_failure_skips_scorers_and_keeps_cause_and_hook_changes() {
        let called = Arc::new(AtomicBool::new(false));
        let scorers: Vec<Box<dyn Scorer<usize, usize>>> =
            vec![Box::new(ProbeScorer(called.clone()))];
        let result = CaseRunner::run(
            Case {
                input: 1,
                ..Default::default()
            },
            &EvalDefinition::new(Box::new(FailTask), scorers).unwrap(),
            None,
            None,
        )
        .await;
        assert!(result.output.is_none());
        assert!(!called.load(Ordering::SeqCst));
        assert_eq!(result.metadata.unwrap()["failed"], true);
        assert_eq!(result.tags.unwrap(), ["failed"]);
        let [CaseError::Task { source }] = result.errors.as_slice() else {
            panic!("task failure must retain its stage")
        };
        assert!(source.downcast_ref::<ApplicationFailure>().is_some());
        assert_eq!(
            serde_json::to_value(&result.errors).unwrap(),
            json!([{"stage":"task","message":"application failure"}])
        );
    }

    struct ParallelScorer {
        name: &'static str,
        gate: Arc<tokio::sync::Barrier>,
        fail: bool,
    }
    #[async_trait]
    impl Scorer<usize, usize> for ParallelScorer {
        fn name(&self) -> &str {
            self.name
        }
        async fn score(&self, _: ScorerArgs<'_, usize, usize>) -> anyhow::Result<Scores> {
            self.gate.wait().await;
            if self.fail {
                return Err(ApplicationFailure.into());
            }
            Ok(vec![Score::new(self.name, 0.75)])
        }
    }
    #[tokio::test]
    async fn scorer_failures_are_named_without_discarding_other_scores_or_stopping_parallel_work() {
        let gate = Arc::new(tokio::sync::Barrier::new(2));
        let scorers: Vec<Box<dyn Scorer<usize, usize>>> = vec![
            Box::new(ParallelScorer {
                name: "bad",
                gate: gate.clone(),
                fail: true,
            }),
            Box::new(ParallelScorer {
                name: "good",
                gate,
                fail: false,
            }),
        ];
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            CaseRunner::run(
                Case {
                    input: 1,
                    ..Default::default()
                },
                &EvalDefinition::new(
                    Box::new(crate::eval::task(|input: &usize, _hooks| {
                        Box::pin(async move { Ok(*input) })
                    })),
                    scorers,
                )
                .unwrap(),
                None,
                None,
            ),
        )
        .await
        .unwrap();
        assert_eq!(result.output, Some(1));
        assert_eq!(result.scores[0].name, "good");
        let [CaseError::Scorer { name, source }] = result.errors.as_slice() else {
            panic!("expected named scorer failure")
        };
        assert_eq!(name, "bad");
        assert!(source.downcast_ref::<ApplicationFailure>().is_some());
        assert_eq!(
            serde_json::to_value(&result.errors).unwrap(),
            json!([{"stage":"scorer","name":"bad","message":"application failure"}])
        );
        let stats = crate::eval::EvalSummary::compute_stats(&[result]);
        assert_eq!(stats[0].name, "good");
        assert_eq!(stats[0].mean, 0.75);
    }
}
