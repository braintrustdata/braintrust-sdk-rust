//! Reusable computation shared by independent evaluation runs.
use std::sync::Arc;

use async_trait::async_trait;

use super::{EvalConfigError, Scorer, ScorerArgs, Scores, Task};

/// A validated task and ordered set of scorers, independent of any client or run.
///
/// Cloning shares the callbacks without cloning inputs or outputs. Callback
/// state is shared across runs; sources, hooks and results belong to each run.
/// Callbacks must therefore support concurrent invocation.
pub struct EvalDefinition<I, O> {
    inner: Arc<DefinitionInner<I, O>>,
}

struct DefinitionInner<I, O> {
    task: Arc<dyn Task<I, O>>,
    scorers: Vec<Arc<dyn Scorer<I, O>>>,
}

impl<I, O> Clone for EvalDefinition<I, O> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<I: Send + Sync + 'static, O: Send + Sync + 'static> EvalDefinition<I, O> {
    /// Validate scorer names and retain the callbacks for reuse.
    ///
    /// Names are captured once, including for custom scorers whose `name()`
    /// could change through interior state. Empty or duplicate names are rejected.
    pub fn new(
        task: Box<dyn Task<I, O>>,
        scorers: Vec<Box<dyn Scorer<I, O>>>,
    ) -> Result<Self, EvalConfigError> {
        let mut names = std::collections::HashSet::new();
        let mut validated: Vec<Arc<dyn Scorer<I, O>>> = Vec::with_capacity(scorers.len());
        for (index, scorer) in scorers.into_iter().enumerate() {
            let name = scorer.name().to_owned();
            if name.trim().is_empty() {
                return Err(EvalConfigError::EmptyScorerName { index });
            }
            if !names.insert(name.clone()) {
                return Err(EvalConfigError::DuplicateScorerName { name });
            }
            validated.push(Arc::new(NamedScorer { name, scorer }));
        }
        Ok(Self {
            inner: Arc::new(DefinitionInner {
                task: Arc::from(task),
                scorers: validated,
            }),
        })
    }
}

impl<I, O> EvalDefinition<I, O> {
    /// Scorer names in definition order, fixed at construction.
    pub fn scorer_names(&self) -> impl ExactSizeIterator<Item = &str> {
        self.inner.scorers.iter().map(|scorer| scorer.name())
    }

    pub(crate) fn task(&self) -> Arc<dyn Task<I, O>> {
        self.inner.task.clone()
    }

    pub(crate) fn scorers(&self) -> &[Arc<dyn Scorer<I, O>>] {
        &self.inner.scorers
    }
}

struct NamedScorer<I, O> {
    name: String,
    scorer: Box<dyn Scorer<I, O>>,
}

#[async_trait]
impl<I: Send + Sync, O: Send + Sync> Scorer<I, O> for NamedScorer<I, O> {
    fn is_blocking(&self) -> bool {
        self.scorer.is_blocking()
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn score_blocking(&self, args: ScorerArgs<'_, I, O>) -> anyhow::Result<Scores> {
        self.scorer.score_blocking(args)
    }

    async fn score(&self, args: ScorerArgs<'_, I, O>) -> anyhow::Result<Scores> {
        self.scorer.score(args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{case_runner::CaseRunner, task, Case, CaseError};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    // No Clone, Serialize, equality or runtime is needed for computation.
    struct Input(usize);
    struct Output(usize);

    #[test]
    fn cloned_definitions_share_callbacks_with_independent_case_state() {
        let calls = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let definition = EvalDefinition::new(
            Box::new(task({
                let calls = calls.clone();
                move |input: &Input, hooks: &crate::eval::TaskHooks<'_, Input, Output>| {
                    let barrier = barrier.clone();
                    let calls = calls.clone();
                    Box::pin(async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        hooks.add_tag(format!("case-{}", input.0));
                        barrier.wait().await;
                        Ok(Output(input.0 + hooks.expected.unwrap().0))
                    })
                }
            })),
            vec![],
        )
        .unwrap();
        let other = definition.clone();
        drop(definition);
        let first = other.clone();
        let case = |input, expected| Case {
            input: Input(input),
            expected: Some(Output(expected)),
            metadata: None,
            tags: None,
            id: None,
            xact_id: None,
            created: None,
        };
        let (one, two) = futures::executor::block_on(async {
            futures::join!(
                CaseRunner::run(case(1, 10), &first, None, None),
                CaseRunner::run(case(2, 20), &other, None, None),
            )
        });
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(one.output.unwrap().0, 11);
        assert_eq!(two.output.unwrap().0, 22);
        assert_eq!(one.tags, Some(vec!["case-1".into()]));
        assert_eq!(two.tags, Some(vec!["case-2".into()]));
        // Reuse the same callbacks after both prior invocations have completed.
        let (one, two) = futures::executor::block_on(async {
            futures::join!(
                CaseRunner::run(case(3, 30), &first, None, None),
                CaseRunner::run(case(4, 40), &other, None, None),
            )
        });
        assert_eq!(one.output.unwrap().0, 33);
        assert_eq!(two.output.unwrap().0, 44);
        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }

    struct ChangingName(Arc<AtomicBool>);
    #[async_trait]
    impl Scorer<usize, usize> for ChangingName {
        fn name(&self) -> &str {
            if self.0.load(Ordering::SeqCst) {
                "changed"
            } else {
                "original"
            }
        }
        async fn score(&self, _: ScorerArgs<'_, usize, usize>) -> anyhow::Result<Scores> {
            anyhow::bail!("scoring failed")
        }
    }

    #[test]
    fn scorer_identity_is_captured_once_for_every_run() {
        let changed = Arc::new(AtomicBool::new(false));
        let definition = EvalDefinition::new(
            Box::new(task(|input: &usize, _hooks| {
                Box::pin(async move { Ok(*input) })
            })),
            vec![Box::new(ChangingName(changed.clone()))],
        )
        .unwrap();
        changed.store(true, Ordering::SeqCst);
        assert_eq!(definition.scorer_names().collect::<Vec<_>>(), ["original"]);
        for definition in [definition.clone(), definition] {
            let result = futures::executor::block_on(CaseRunner::run(
                Case {
                    input: 1,
                    ..Default::default()
                },
                &definition,
                None,
                None,
            ));
            assert!(
                matches!(&result.errors[..], [CaseError::Scorer {name, source}] if name == "original" && source.to_string() == "scoring failed")
            );
        }
    }
}
