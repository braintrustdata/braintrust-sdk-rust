use async_trait::async_trait;
use std::marker::PhantomData;

use crate::Result;

use super::types::{Score, ScorerArgs, Scores};

/// Scorer trait for evaluating task outputs
///
/// Scorers compare the task output against expected values and produce numeric scores.
/// Only async version is needed - SyncEvaluator uses this via Runtime::block_on().
#[async_trait]
pub trait Scorer<I, O>: Send + Sync {
    /// Name of this scorer (used in results)
    fn name(&self) -> &str;

    /// Score the output
    async fn score(&self, args: ScorerArgs<'_, I, O>) -> Result<Scores>;
}

/// Wrapper for sync scoring functions
///
/// This allows sync functions to be used as scorers in both async and sync evaluators.
pub struct SyncFnScorer<I, O, F>
where
    F: Fn(ScorerArgs<'_, I, O>) -> Result<Scores> + Send + Sync,
{
    name: String,
    func: F,
    _phantom: PhantomData<(I, O)>,
}

impl<I, O, F> SyncFnScorer<I, O, F>
where
    F: Fn(ScorerArgs<'_, I, O>) -> Result<Scores> + Send + Sync,
{
    pub fn new(name: impl Into<String>, func: F) -> Self {
        Self {
            name: name.into(),
            func,
            _phantom: PhantomData,
        }
    }
}

#[async_trait]
impl<I, O, F> Scorer<I, O> for SyncFnScorer<I, O, F>
where
    F: Fn(ScorerArgs<'_, I, O>) -> Result<Scores> + Send + Sync,
    I: Send + Sync,
    O: Send + Sync,
{
    fn name(&self) -> &str {
        &self.name
    }

    async fn score(&self, args: ScorerArgs<'_, I, O>) -> Result<Scores> {
        (self.func)(args)
    }
}

/// Built-in scorer: Exact match between output and expected
pub struct ExactMatch {
    name: String,
}

impl ExactMatch {
    pub fn new() -> Self {
        Self {
            name: "exact_match".to_string(),
        }
    }

    pub fn with_name(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

impl Default for ExactMatch {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl<I, O> Scorer<I, O> for ExactMatch
where
    O: PartialEq + Send + Sync,
    I: Send + Sync,
{
    fn name(&self) -> &str {
        &self.name
    }

    async fn score(&self, args: ScorerArgs<'_, I, O>) -> Result<Scores> {
        let score = match args.expected {
            Some(expected) if expected == args.output => 1.0,
            Some(_) => 0.0,
            None => 0.0, // No expected value means we can't score
        };

        Ok(vec![Score::new(self.name.clone(), score)])
    }
}

/// Built-in scorer: Levenshtein distance for string comparison
pub struct LevenshteinScorer {
    name: String,
    /// If true, normalize score to 0-1 range (default: true)
    normalize: bool,
}

impl LevenshteinScorer {
    pub fn new() -> Self {
        Self {
            name: "levenshtein".to_string(),
            normalize: true,
        }
    }

    pub fn with_name(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            normalize: true,
        }
    }

    pub fn normalized(mut self, normalize: bool) -> Self {
        self.normalize = normalize;
        self
    }

    /// Calculate Levenshtein distance between two strings.
    ///
    /// Uses a single-row vector for O(n) space complexity.
    fn levenshtein_distance(s1: &str, s2: &str) -> usize {
        let s1_chars: Vec<char> = s1.chars().collect();
        let s2_chars: Vec<char> = s2.chars().collect();
        let len1 = s1_chars.len();
        let len2 = s2_chars.len();

        if len1 == 0 {
            return len2;
        }
        if len2 == 0 {
            return len1;
        }

        let mut row: Vec<usize> = (0..=len2).collect();

        for (i, c1) in s1_chars.iter().enumerate() {
            let mut prev = i;
            row[0] = i + 1;
            for (j, c2) in s2_chars.iter().enumerate() {
                let tmp = row[j + 1];
                row[j + 1] = if c1 == c2 {
                    prev
                } else {
                    1 + prev.min(tmp).min(row[j])
                };
                prev = tmp;
            }
        }

        row[len2]
    }
}

impl Default for LevenshteinScorer {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl<I> Scorer<I, String> for LevenshteinScorer
where
    I: Send + Sync,
{
    fn name(&self) -> &str {
        &self.name
    }

    async fn score(&self, args: ScorerArgs<'_, I, String>) -> Result<Scores> {
        let expected = match args.expected {
            Some(e) => e,
            None => return Ok(vec![Score::new(self.name.clone(), 0.0)]),
        };

        let distance = Self::levenshtein_distance(args.output, expected);

        let score = if self.normalize {
            let max_len = std::cmp::max(args.output.chars().count(), expected.chars().count());
            if max_len == 0 {
                1.0
            } else {
                1.0 - (distance as f64 / max_len as f64)
            }
        } else {
            distance as f64
        };

        Ok(vec![Score::new(self.name.clone(), score)])
    }
}

// Note: For &str output types, users should convert to String or use a custom scorer
// due to lifetime complexity with async_trait

/// Built-in scorer: Numeric difference for numeric outputs
pub struct NumericDiffScorer {
    name: String,
    /// If true, return absolute difference; if false, return relative error
    absolute: bool,
}

impl NumericDiffScorer {
    pub fn new() -> Self {
        Self {
            name: "numeric_diff".to_string(),
            absolute: true,
        }
    }

    pub fn with_name(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            absolute: true,
        }
    }

    pub fn absolute(mut self, absolute: bool) -> Self {
        self.absolute = absolute;
        self
    }
}

impl Default for NumericDiffScorer {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl<I> Scorer<I, f64> for NumericDiffScorer
where
    I: Send + Sync,
{
    fn name(&self) -> &str {
        &self.name
    }

    async fn score(&self, args: ScorerArgs<'_, I, f64>) -> Result<Scores> {
        let expected = match args.expected {
            Some(e) => e,
            None => return Ok(vec![Score::new(self.name.clone(), 0.0)]),
        };

        let score = if self.absolute {
            (args.output - expected).abs()
        } else if *expected != 0.0 {
            ((args.output - expected) / expected).abs()
        } else {
            args.output.abs()
        };

        Ok(vec![Score::new(self.name.clone(), score)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_exact_match() {
        let scorer = ExactMatch::new();

        let args = ScorerArgs {
            input: &"hello".to_string(),
            output: &"HELLO".to_string(),
            expected: Some(&"HELLO".to_string()),
            metadata: None,
        };

        let scores = scorer.score(args).await.unwrap();
        assert_eq!(scores.len(), 1);
        assert_eq!(scores[0].score, 1.0);

        let args = ScorerArgs {
            input: &"hello".to_string(),
            output: &"HELLO".to_string(),
            expected: Some(&"hello".to_string()),
            metadata: None,
        };

        let scores = scorer.score(args).await.unwrap();
        assert_eq!(scores[0].score, 0.0);
    }

    #[tokio::test]
    async fn test_levenshtein() {
        let scorer = LevenshteinScorer::new();

        let args = ScorerArgs {
            input: &"test".to_string(),
            output: &"hello".to_string(),
            expected: Some(&"hallo".to_string()),
            metadata: None,
        };

        let scores = scorer.score(args).await.unwrap();
        assert_eq!(scores.len(), 1);
        // Distance of 1 in a 5-char string = 0.8 similarity
        assert!((scores[0].score - 0.8).abs() < 0.01);
    }

    #[tokio::test]
    async fn test_numeric_diff() {
        let scorer = NumericDiffScorer::new();

        let args = ScorerArgs {
            input: &"test".to_string(),
            output: &10.0,
            expected: Some(&8.0),
            metadata: None,
        };

        let scores = scorer.score(args).await.unwrap();
        assert_eq!(scores.len(), 1);
        assert_eq!(scores[0].score, 2.0);
    }
}
