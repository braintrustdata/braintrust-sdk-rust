//! Console presentation, separate from execution and recording.
use super::{executor::EvalReporter, EvalResult, EvalSummary};

pub(crate) struct TerminalReporter {
    pub quiet: bool,
}
impl<I, O> EvalReporter<I, O> for TerminalReporter {
    fn started(&mut self, name: &str) {
        if !self.quiet {
            println!("Running evaluation: {name}");
        }
    }
    fn case_finished(&mut self, index: usize, result: &EvalResult<I, O>) {
        if self.quiet {
            return;
        }
        let scores = result
            .scores
            .iter()
            .map(|score| format!("{}={:.3}", score.name, score.score))
            .collect::<Vec<_>>()
            .join(", ");
        println!("Case {}: {}", index + 1, scores);
        for error in &result.errors {
            eprintln!("Case {}: {}", index + 1, error);
        }
    }
    fn finished(&mut self, summary: &EvalSummary<I, O>) {
        if self.quiet {
            return;
        }
        println!("\nEvaluation complete!");
        println!("Total cases: {}", summary.total_cases);
        println!("Successful: {}", summary.successful_cases);
        println!("Failed: {}", summary.failed_cases);
        println!("\nScore summary:");
        for stat in &summary.score_stats {
            println!(
                "  {}: mean={:.3}, min={:.3}, max={:.3}, count={}",
                stat.name, stat.mean, stat.min, stat.max, stat.count
            );
        }
    }
}
