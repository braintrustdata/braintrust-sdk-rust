use async_trait::async_trait;
use futures::future::BoxFuture;
use std::marker::PhantomData;

use anyhow::Result;

use super::types::TaskHooks;

/// Task trait for transforming inputs to outputs
///
/// Tasks can be async functions, closures, or custom structs implementing this trait.
/// Callback errors use `anyhow::Result` to preserve application error causes.
#[async_trait]
pub trait Task<I, O>: Send + Sync {
    /// Execute the task on the given input
    async fn run(&self, input: &I, hooks: &TaskHooks<'_, I, O>) -> Result<O>;
}

/// Wrapper for sync task functions
///
/// This allows sync functions to be used as tasks in both async and sync evaluators.
/// The function runs synchronously but is wrapped in an async trait implementation.
pub struct SyncFnTask<I, O, F>
where
    F: Fn(&I, &TaskHooks<'_, I, O>) -> Result<O> + Send + Sync,
{
    func: F,
    _phantom: PhantomData<(I, O)>,
}

impl<I, O, F> SyncFnTask<I, O, F>
where
    F: Fn(&I, &TaskHooks<'_, I, O>) -> Result<O> + Send + Sync,
{
    pub fn new(func: F) -> Self {
        Self {
            func,
            _phantom: PhantomData,
        }
    }
}

#[async_trait]
impl<I, O, F> Task<I, O> for SyncFnTask<I, O, F>
where
    F: Fn(&I, &TaskHooks<'_, I, O>) -> Result<O> + Send + Sync,
    I: Send + Sync,
    O: Send + Sync,
{
    async fn run(&self, input: &I, hooks: &TaskHooks<'_, I, O>) -> Result<O> {
        (self.func)(input, hooks)
    }
}

/// Supply type context for a reusable async task closure without wrapping it.
/// The returned callable directly implements [`Task`]. Its future may borrow
/// this invocation's input and hooks across await points.
pub fn task<I, O, F>(callback: F) -> F
where
    F: for<'a> Fn(&'a I, &'a TaskHooks<'a, I, O>) -> BoxFuture<'a, Result<O>> + Send + Sync,
{
    callback
}

#[async_trait]
impl<I, O, F> Task<I, O> for F
where
    F: for<'a> Fn(&'a I, &'a TaskHooks<'a, I, O>) -> BoxFuture<'a, Result<O>> + Send + Sync,
    I: Send + Sync,
    O: Send + Sync,
{
    async fn run(&self, input: &I, hooks: &TaskHooks<'_, I, O>) -> Result<O> {
        self(input, hooks).await
    }
}

/// Simple wrapper for tasks that don't need hooks
pub struct SimpleFnTask<I, O, F>
where
    F: Fn(&I) -> Result<O> + Send + Sync,
{
    func: F,
    _phantom: PhantomData<(I, O)>,
}

impl<I, O, F> SimpleFnTask<I, O, F>
where
    F: Fn(&I) -> Result<O> + Send + Sync,
{
    pub fn new(func: F) -> Self {
        Self {
            func,
            _phantom: PhantomData,
        }
    }
}

#[async_trait]
impl<I, O, F> Task<I, O> for SimpleFnTask<I, O, F>
where
    F: Fn(&I) -> Result<O> + Send + Sync,
    I: Send + Sync,
    O: Send + Sync,
{
    async fn run(&self, input: &I, _hooks: &TaskHooks<'_, I, O>) -> Result<O> {
        (self.func)(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_simple_fn_task() {
        let task = SimpleFnTask::new(|input: &String| Ok(input.to_uppercase()));

        let task: Box<dyn Task<String, String>> = Box::new(task);
        let input = String::from("hello");
        let hooks = TaskHooks::local(None);
        assert_eq!(task.run(&input, &hooks).await.unwrap(), "HELLO");
    }
}
