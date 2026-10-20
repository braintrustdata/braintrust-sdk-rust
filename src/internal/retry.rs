//! Retry mechanics shared by protocol-specific callers.
//!
//! Callers decide replay safety, classification and diagnostics. This module
//! only bounds attempts and advances a supplied delay schedule; it retains no
//! failed operations once the run ends.

use std::{future::Future, iter::Peekable, time::Duration};

use backoff::{backoff::Backoff, ExponentialBackoffBuilder};
use futures::future::BoxFuture;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Attempt {
    /// Zero-based index; total attempts are configured retries plus one.
    pub(crate) index: usize,
}

pub(crate) trait Sleeper {
    fn sleep(&self, delay: Duration) -> BoxFuture<'_, ()>;
}
pub(crate) struct TokioSleeper;
impl Sleeper for TokioSleeper {
    fn sleep(&self, delay: Duration) -> BoxFuture<'_, ()> {
        Box::pin(tokio::time::sleep(delay))
    }
}

pub(crate) struct RetryOutcome<T, E> {
    pub(crate) result: Result<T, E>,
    pub(crate) attempts: usize,
}

pub(crate) struct RetryExecutor<B: Iterator<Item = Duration>, S = TokioSleeper> {
    delays: Peekable<B>,
    sleeper: S,
    max_retries: usize,
    next_index: Option<usize>,
}
impl<B: Iterator<Item = Duration>> RetryExecutor<B> {
    pub(crate) fn new(max_retries: usize, delays: B) -> Self {
        Self::with_sleeper(max_retries, delays, TokioSleeper)
    }
}
impl<B: Iterator<Item = Duration>, S: Sleeper> RetryExecutor<B, S> {
    fn with_sleeper(max_retries: usize, delays: B, sleeper: S) -> Self {
        Self {
            delays: delays.peekable(),
            sleeper,
            max_retries,
            next_index: Some(0),
        }
    }

    /// Advance a stateful protocol orchestrator without moving its state into
    /// an operation factory. Breaking/dropping this cursor stops further retries.
    pub(crate) async fn next(&mut self) -> Option<Attempt> {
        let index = self.next_index.take()?;
        if index > 0 {
            let delay = self.delays.next()?;
            self.sleeper.sleep(delay).await;
        }
        self.next_index = (index < self.max_retries).then(|| index + 1);
        Some(Attempt { index })
    }

    pub(crate) async fn run<T, E, F, Fut, C, N>(
        mut self,
        operation: F,
        retryable: C,
        on_retry: N,
    ) -> RetryOutcome<T, E>
    where
        F: FnMut(Attempt) -> Fut,
        Fut: Future<Output = Result<T, E>>,
        C: FnMut(&E) -> bool,
        N: FnMut(Attempt, &E, Duration),
    {
        self.run_shared(operation, retryable, on_retry).await
    }

    /// Execute one stage, sharing the remaining retry allowance with later
    /// stages. Successful stages do not consume retries or reset the schedule.
    pub(crate) async fn run_shared<T, E, F, Fut, C, N>(
        &mut self,
        mut operation: F,
        mut retryable: C,
        mut on_retry: N,
    ) -> RetryOutcome<T, E>
    where
        F: FnMut(Attempt) -> Fut,
        Fut: Future<Output = Result<T, E>>,
        C: FnMut(&E) -> bool,
        N: FnMut(Attempt, &E, Duration),
    {
        if self.next_index == Some(0) {
            self.next().await;
        }
        let mut attempts: usize = 0;
        loop {
            let attempt = Attempt { index: attempts };
            attempts = attempts.saturating_add(1);
            match operation(attempt).await {
                Ok(value) => {
                    return RetryOutcome {
                        result: Ok(value),
                        attempts,
                    }
                }
                Err(error) => {
                    if !retryable(&error) || self.next_index.is_none() {
                        return RetryOutcome {
                            result: Err(error),
                            attempts,
                        };
                    }
                    let Some(delay) = self.delays.peek().copied() else {
                        return RetryOutcome {
                            result: Err(error),
                            attempts,
                        };
                    };
                    on_retry(attempt, &error, delay);
                    if self.next().await.is_none() {
                        return RetryOutcome {
                            result: Err(error),
                            attempts,
                        };
                    }
                }
            }
        }
    }
}

/// Deterministic capped doubling; the first delay follows the initial attempt.
pub(crate) fn exponential(initial: Duration, max: Duration) -> impl Iterator<Item = Duration> {
    std::iter::successors(Some(initial.min(max)), move |delay| {
        Some(delay.saturating_mul(2).min(max))
    })
}

/// Existing jittered upload policy, separate from executing HTTP operations.
pub(crate) fn jittered_exponential(
    initial: Duration,
    max: Duration,
    randomization: f64,
) -> impl Iterator<Item = Duration> {
    let mut backoff = ExponentialBackoffBuilder::new()
        .with_initial_interval(initial)
        .with_multiplier(2.0)
        .with_randomization_factor(randomization)
        .with_max_interval(max)
        .with_max_elapsed_time(None)
        .build();
    std::iter::from_fn(move || backoff.next_backoff())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Default, Clone)]
    struct FakeSleeper {
        delays: Arc<Mutex<Vec<Duration>>>,
    }
    impl Sleeper for FakeSleeper {
        fn sleep(&self, delay: Duration) -> BoxFuture<'_, ()> {
            self.delays.lock().unwrap().push(delay);
            Box::pin(async {})
        }
    }

    #[tokio::test]
    async fn stages_share_retries_without_resetting_the_delay_schedule() {
        let sleeper = FakeSleeper::default();
        let mut executor = RetryExecutor::with_sleeper(
            2,
            exponential(Duration::from_secs(1), Duration::from_secs(8)),
            sleeper.clone(),
        );
        for _ in 0..2 {
            let outcome = executor
                .run_shared(
                    |attempt| async move {
                        if attempt.index == 0 {
                            Err("transient")
                        } else {
                            Ok(())
                        }
                    },
                    |_| true,
                    |_, _, _| {},
                )
                .await;
            assert_eq!(outcome.attempts, 2);
            assert_eq!(outcome.result, Ok(()));
        }
        let outcome = executor
            .run_shared(
                |_| async { Err::<(), _>("exhausted") },
                |_| true,
                |_, _, _| {},
            )
            .await;
        assert_eq!(outcome.attempts, 1);
        assert_eq!(outcome.result, Err("exhausted"));
        assert_eq!(
            *sleeper.delays.lock().unwrap(),
            vec![Duration::from_secs(1), Duration::from_secs(2)]
        );
    }

    #[tokio::test]
    async fn zero_retries_still_attempts_once_without_sleeping() {
        let sleeper = FakeSleeper::default();
        let seen = sleeper.clone();
        let outcome = RetryExecutor::with_sleeper(
            0,
            exponential(Duration::from_secs(1), Duration::from_secs(30)),
            sleeper,
        )
        .run(
            |a| async move {
                assert_eq!(a.index, 0);
                Err::<(), _>("original")
            },
            |_| true,
            |_, _, _| panic!("no retry"),
        )
        .await;
        assert_eq!(outcome.result, Err("original"));
        assert_eq!(outcome.attempts, 1);
        assert!(seen.delays.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn retries_are_additional_attempts_with_capped_delays_and_notifications() {
        let sleeper = FakeSleeper::default();
        let seen = sleeper.clone();
        let mut notifications = Vec::new();
        let outcome = RetryExecutor::with_sleeper(
            4,
            exponential(Duration::from_secs(1), Duration::from_secs(3)),
            sleeper,
        )
        .run(
            |a| async move {
                if a.index == 4 {
                    Ok(a.index)
                } else {
                    Err(a.index)
                }
            },
            |_| true,
            |attempt, error, delay| notifications.push((attempt.index, *error, delay)),
        )
        .await;
        assert_eq!(outcome.result, Ok(4));
        assert_eq!(outcome.attempts, 5);
        let delays = vec![
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(3),
            Duration::from_secs(3),
        ];
        assert_eq!(*seen.delays.lock().unwrap(), delays);
        assert_eq!(
            notifications.iter().map(|(_, _, d)| *d).collect::<Vec<_>>(),
            delays
        );
    }

    #[tokio::test]
    async fn permanent_errors_stop_without_sleeping_or_requiring_clone() {
        struct OwnedError(Box<u8>);
        let sleeper = FakeSleeper::default();
        let seen = sleeper.clone();
        let outcome = RetryExecutor::with_sleeper(
            10,
            exponential(Duration::from_secs(1), Duration::from_secs(30)),
            sleeper,
        )
        .run(
            |_| async { Err::<(), _>(OwnedError(Box::new(42))) },
            |_| false,
            |_, _, _| panic!("permanent"),
        )
        .await;
        assert_eq!(*outcome.result.err().unwrap().0, 42);
        assert_eq!(outcome.attempts, 1);
        assert!(seen.delays.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn finite_schedule_returns_the_last_error_without_an_extra_attempt() {
        let sleeper = FakeSleeper::default();
        let seen = sleeper.clone();
        let outcome =
            RetryExecutor::with_sleeper(20, [Duration::from_secs(7)].into_iter(), sleeper)
                .run(
                    |a| async move { Err::<(), _>(a.index) },
                    |_| true,
                    |_, _, _| {},
                )
                .await;
        assert_eq!(outcome.result, Err(1));
        assert_eq!(outcome.attempts, 2);
        assert_eq!(*seen.delays.lock().unwrap(), [Duration::from_secs(7)]);
    }

    #[tokio::test]
    async fn dropping_a_retry_wait_does_not_schedule_another_attempt() {
        struct PendingSleeper;
        impl Sleeper for PendingSleeper {
            fn sleep(&self, _: Duration) -> BoxFuture<'_, ()> {
                Box::pin(std::future::pending())
            }
        }
        let attempts = std::sync::atomic::AtomicUsize::new(0);
        let mut run = Box::pin(
            RetryExecutor::with_sleeper(
                3,
                exponential(Duration::from_secs(1), Duration::from_secs(3)),
                PendingSleeper,
            )
            .run(
                |_| {
                    attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    async { Err::<(), _>(()) }
                },
                |_| true,
                |_, _, _| {},
            ),
        );
        assert!(futures::poll!(run.as_mut()).is_pending());
        drop(run);
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
