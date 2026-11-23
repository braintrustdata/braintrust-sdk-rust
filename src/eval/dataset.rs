use async_trait::async_trait;
use futures::stream::Stream;

use crate::Result;

use super::types::Case;

/// Iterator trait for evaluation test cases
///
/// This trait allows different dataset sources (in-memory vectors, API-backed datasets, etc.)
/// to provide test cases to the evaluator. Follows the Go SDK pattern where Dataset is
/// an interface/trait rather than a concrete type.
#[async_trait]
pub trait Dataset<I, O>: Send + Sync
where
    I: Clone,
    O: Clone,
{
    /// Get the next case from the dataset
    async fn next(&mut self) -> Result<Option<Case<I, O>>>;

    /// Get the dataset ID (if backed by Braintrust API)
    fn id(&self) -> Option<String> {
        None
    }

    /// Get the dataset version (if backed by Braintrust API)
    fn version(&self) -> Option<String> {
        None
    }
}

/// In-memory dataset backed by a vector
pub struct VecDataset<I, O>
where
    I: Clone,
    O: Clone,
{
    cases: Vec<Case<I, O>>,
    index: usize,
}

impl<I, O> VecDataset<I, O>
where
    I: Clone,
    O: Clone,
{
    pub fn new(cases: Vec<Case<I, O>>) -> Self {
        Self { cases, index: 0 }
    }
}

#[async_trait]
impl<I, O> Dataset<I, O> for VecDataset<I, O>
where
    I: Send + Sync + Clone,
    O: Send + Sync + Clone,
{
    async fn next(&mut self) -> Result<Option<Case<I, O>>> {
        if self.index < self.cases.len() {
            let case = self.cases[self.index].clone();
            self.index += 1;
            Ok(Some(case))
        } else {
            Ok(None)
        }
    }
}

/// Implement Dataset for any sync iterator over Cases
///
/// This allows using standard iterators as datasets without manual wrapping.
pub struct IterDataset<I, O, T>
where
    I: Clone,
    O: Clone,
    T: Iterator<Item = Case<I, O>>,
{
    iter: T,
}

impl<I, O, T> IterDataset<I, O, T>
where
    I: Clone,
    O: Clone,
    T: Iterator<Item = Case<I, O>>,
{
    pub fn new(iter: T) -> Self {
        Self { iter }
    }
}

#[async_trait]
impl<I, O, T> Dataset<I, O> for IterDataset<I, O, T>
where
    T: Iterator<Item = Case<I, O>> + Send + Sync,
    I: Send + Sync + Clone,
    O: Send + Sync + Clone,
{
    async fn next(&mut self) -> Result<Option<Case<I, O>>> {
        Ok(self.iter.next())
    }
}

/// Implement Dataset for async streams
pub struct StreamDataset<I, O, S>
where
    I: Clone,
    O: Clone,
    S: Stream<Item = Case<I, O>> + Send + Unpin,
{
    stream: S,
}

impl<I, O, S> StreamDataset<I, O, S>
where
    I: Clone,
    O: Clone,
    S: Stream<Item = Case<I, O>> + Send + Unpin,
{
    pub fn new(stream: S) -> Self {
        Self { stream }
    }
}

#[async_trait]
impl<I, O, S> Dataset<I, O> for StreamDataset<I, O, S>
where
    S: Stream<Item = Case<I, O>> + Send + Sync + Unpin,
    I: Send + Sync + Clone,
    O: Send + Sync + Clone,
{
    async fn next(&mut self) -> Result<Option<Case<I, O>>> {
        use futures::stream::StreamExt;
        Ok(self.stream.next().await)
    }
}

/// Convenience conversion from `Vec<Case>` to boxed Dataset
impl<I, O> From<Vec<Case<I, O>>> for Box<dyn Dataset<I, O>>
where
    I: Send + Sync + Clone + 'static,
    O: Send + Sync + Clone + 'static,
{
    fn from(cases: Vec<Case<I, O>>) -> Self {
        Box::new(VecDataset::new(cases))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_vec_dataset() {
        let cases = vec![
            Case {
                input: "hello",
                expected: Some("HELLO"),
                ..Default::default()
            },
            Case {
                input: "world",
                expected: Some("WORLD"),
                ..Default::default()
            },
        ];

        let mut dataset = VecDataset::new(cases);

        let case1 = dataset.next().await.unwrap();
        assert!(case1.is_some());
        assert_eq!(case1.unwrap().input, "hello");

        let case2 = dataset.next().await.unwrap();
        assert!(case2.is_some());
        assert_eq!(case2.unwrap().input, "world");

        let case3 = dataset.next().await.unwrap();
        assert!(case3.is_none());
    }

    #[tokio::test]
    async fn test_vec_to_boxed_dataset() {
        let cases = vec![Case {
            input: "test",
            expected: Some("TEST"),
            ..Default::default()
        }];

        let mut dataset: Box<dyn Dataset<&str, &str>> = cases.into();
        let case = dataset.next().await.unwrap();
        assert!(case.is_some());
        assert_eq!(case.unwrap().input, "test");
    }
}
