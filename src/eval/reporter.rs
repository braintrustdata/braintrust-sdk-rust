//! Run-owned output adapters. Reporting never owns computation or recording.
use std::io::{self, Write};

use serde::Serialize;

use super::{EvalResult, EvalRunFailure, EvalSummary};

/// A failure to present evaluation progress or results.
#[derive(Debug, thiserror::Error)]
pub enum ReportError {
    #[error("output failed: {0}")]
    Io(#[from] io::Error),
    #[error("result serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Where a reporter failed. Case indices in run failures are zero-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportStage {
    Started,
    CaseFinished,
    Finished,
}

/// Per-run presentation, without serialization bounds on local computation.
///
/// Callbacks run sequentially on the run's execution binding and must return
/// promptly. Case notifications follow completion order and carry source indices;
/// the final summary is in source order. `finished` also receives partial summaries
/// after source or recording failures.
///
/// The first returned error disables this reporter for the rest of the run.
/// Computation and recording continue; the completion reports a `Reporting`
/// failure with the computed summary in `EvalRunError::partial`. Reporter panics
/// follow the run's ordinary worker-loss behavior.
pub trait EvalReporter<I, O>: Send {
    fn started(&mut self, _name: &str) -> Result<(), ReportError> {
        Ok(())
    }
    fn case_finished(
        &mut self,
        _index: usize,
        _result: &EvalResult<I, O>,
    ) -> Result<(), ReportError> {
        Ok(())
    }
    fn finished(&mut self, _summary: &EvalSummary<I, O>) -> Result<(), ReportError> {
        Ok(())
    }
}

/// Suppress all progress and result output.
pub struct SilentReporter;
impl<I, O> EvalReporter<I, O> for SilentReporter {}

/// Human-readable progress on stdout and case errors on stderr.
pub struct TerminalReporter {
    output: Box<dyn Write + Send>,
    errors: Box<dyn Write + Send>,
}
impl Default for TerminalReporter {
    fn default() -> Self {
        Self::with_writers(io::stdout(), io::stderr())
    }
}
impl TerminalReporter {
    pub fn with_writers(
        output: impl Write + Send + 'static,
        errors: impl Write + Send + 'static,
    ) -> Self {
        Self {
            output: Box::new(output),
            errors: Box::new(errors),
        }
    }
}
impl<I, O> EvalReporter<I, O> for TerminalReporter {
    fn started(&mut self, name: &str) -> Result<(), ReportError> {
        writeln!(self.output, "Running evaluation: {name}")?;
        Ok(())
    }
    fn case_finished(
        &mut self,
        index: usize,
        result: &EvalResult<I, O>,
    ) -> Result<(), ReportError> {
        let scores = result
            .scores
            .iter()
            .map(|score| format!("{}={:.3}", score.name, score.score))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(self.output, "Case {}: {}", index + 1, scores)?;
        for error in &result.errors {
            writeln!(self.errors, "Case {}: {}", index + 1, error)?;
        }
        Ok(())
    }
    fn finished(&mut self, summary: &EvalSummary<I, O>) -> Result<(), ReportError> {
        writeln!(self.output, "\nEvaluation results:")?;
        writeln!(self.output, "Total cases: {}", summary.total_cases)?;
        writeln!(self.output, "Successful: {}", summary.successful_cases)?;
        writeln!(self.output, "Failed: {}", summary.failed_cases)?;
        writeln!(self.output, "\nScore summary:")?;
        for stat in &summary.score_stats {
            writeln!(
                self.output,
                "  {}: mean={:.3}, min={:.3}, max={:.3}, count={}",
                stat.name, stat.mean, stat.min, stat.max, stat.count
            )?;
        }
        self.output.flush()?;
        self.errors.flush()?;
        Ok(())
    }
}

/// Emit one serialized summary followed by a newline, with no progress text.
/// Serialization is required only when this adapter is selected. Serialization
/// errors are returned before writing a line; I/O failures may leave a partial line.
///
/// ```no_run
/// use braintrust_sdk_rust::eval::{EvalOpts, EvalRequest, JsonlReporter};
/// # fn example(options: EvalOpts<usize, usize>) {
/// let completion = braintrust_sdk_rust::eval::run(
///     EvalRequest::local(options).with_reporter(JsonlReporter::new(std::io::stdout())),
/// );
/// # }
/// ```
pub struct JsonlReporter<W> {
    writer: W,
}
impl<W> JsonlReporter<W> {
    pub fn new(writer: W) -> Self {
        Self { writer }
    }
}
impl<I: Serialize, O: Serialize, W: Write + Send> EvalReporter<I, O> for JsonlReporter<W> {
    fn finished(&mut self, summary: &EvalSummary<I, O>) -> Result<(), ReportError> {
        let line = serde_json::to_vec(summary)?;
        self.writer.write_all(&line)?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        Ok(())
    }
}

/// Keep reporting failures out of the scheduler's stop-reading condition.
/// Disable after the first error so broken output cannot accumulate errors per case.
pub(crate) struct ReporterSession<'a, I, O> {
    reporter: Option<&'a mut dyn EvalReporter<I, O>>,
    failure: Option<EvalRunFailure>,
}
impl<'a, I, O> ReporterSession<'a, I, O> {
    pub fn new(reporter: &'a mut dyn EvalReporter<I, O>) -> Self {
        Self {
            reporter: Some(reporter),
            failure: None,
        }
    }
    fn observe(
        &mut self,
        outcome: Result<(), ReportError>,
        stage: ReportStage,
        case_index: Option<usize>,
    ) {
        if let Err(source) = outcome {
            self.reporter = None;
            self.failure = Some(EvalRunFailure::Reporting {
                stage,
                case_index,
                source,
            });
        }
    }
    pub fn started(&mut self, name: &str) {
        if let Some(reporter) = self.reporter.as_mut() {
            let outcome = reporter.started(name);
            self.observe(outcome, ReportStage::Started, None);
        }
    }
    pub fn case_finished(&mut self, index: usize, result: &EvalResult<I, O>) {
        if let Some(reporter) = self.reporter.as_mut() {
            let outcome = reporter.case_finished(index, result);
            self.observe(outcome, ReportStage::CaseFinished, Some(index));
        }
    }
    pub fn finished(&mut self, summary: &EvalSummary<I, O>) -> Option<EvalRunFailure> {
        if let Some(reporter) = self.reporter.as_mut() {
            let outcome = reporter.finished(summary);
            self.observe(outcome, ReportStage::Finished, None);
        }
        self.failure.take()
    }
}
