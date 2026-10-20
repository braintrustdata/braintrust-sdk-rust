//! Owned, fallible case sources, independent of recording and presentation.
//!
//! ```no_run
//! use braintrust_sdk_rust::eval::{Case, CaseSource, IterCaseSource};
//!
//! # async fn example() -> anyhow::Result<()> {
//! let cases = (0..3).map(|input| Ok(Case::<usize, usize> {
//!     input, ..Default::default()
//! }));
//! let mut source: Box<dyn CaseSource<usize, usize>> =
//!     Box::new(IterCaseSource::new(cases));
//! assert_eq!(source.next().await?.unwrap().input, 0);
//! # Ok(())
//! # }
//! ```
use super::Case;
use async_trait::async_trait;
use futures::{Stream, StreamExt};
use serde::Serialize;
use std::pin::Pin;

/// Application errors from a source retain their original cause.
pub type SourceError = anyhow::Error;

/// Dataset identity captured by a source and retained with its run result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DatasetVersionRef {
    pub dataset_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// One run owns its source and reads it sequentially. Sources need Send,
/// without requiring Sync, cloning user values, or serializing local cases.
#[async_trait]
pub trait CaseSource<I, O>: Send {
    async fn next(&mut self) -> Result<Option<Case<I, O>>, SourceError>;
    fn provenance(&self) -> Option<DatasetVersionRef> {
        None
    }
}

/// Move cases out of an in-memory vector without retaining consumed values.
pub struct VecCaseSource<I, O> {
    cases: std::vec::IntoIter<Case<I, O>>,
}
impl<I, O> VecCaseSource<I, O> {
    pub fn new(cases: Vec<Case<I, O>>) -> Self {
        Self {
            cases: cases.into_iter(),
        }
    }
}
#[async_trait]
impl<I: Send, O: Send> CaseSource<I, O> for VecCaseSource<I, O> {
    async fn next(&mut self) -> Result<Option<Case<I, O>>, SourceError> {
        Ok(self.cases.next())
    }
}

/// Adapt a fallible iterator. For infallible iterators, use `iter.map(Ok)`.
pub struct IterCaseSource<T> {
    iter: T,
}
impl<T> IterCaseSource<T> {
    pub fn new(iter: T) -> Self {
        Self { iter }
    }
}
#[async_trait]
impl<I, O, T> CaseSource<I, O> for IterCaseSource<T>
where
    T: Iterator<Item = Result<Case<I, O>, SourceError>> + Send,
{
    async fn next(&mut self) -> Result<Option<Case<I, O>>, SourceError> {
        self.iter.next().transpose()
    }
}

/// Adapt a fallible stream, including streams that are not Unpin or Sync.
/// For infallible streams, use `stream.map(Ok)`.
pub struct StreamCaseSource<S> {
    stream: Pin<Box<S>>,
}
impl<S> StreamCaseSource<S> {
    pub fn new(stream: S) -> Self {
        Self {
            stream: Box::pin(stream),
        }
    }
}
#[async_trait]
impl<I, O, S> CaseSource<I, O> for StreamCaseSource<S>
where
    S: Stream<Item = Result<Case<I, O>, SourceError>> + Send,
{
    async fn next(&mut self) -> Result<Option<Case<I, O>>, SourceError> {
        self.stream.next().await.transpose()
    }
}
impl<I: Send + 'static, O: Send + 'static> From<Vec<Case<I, O>>> for Box<dyn CaseSource<I, O>> {
    fn from(cases: Vec<Case<I, O>>) -> Self {
        Box::new(VecCaseSource::new(cases))
    }
}
