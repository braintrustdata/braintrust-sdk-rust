use super::observation::DeliveryError;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

pub(super) struct Loss {
    pub submissions: Vec<usize>,
    pub preparation: bool,
}
type BatchSubmissions = Arc<[Vec<usize>]>;

#[derive(Clone)]
pub(super) enum Outcome {
    Pending,
    Delivered,
    Failed(DeliveryError),
}

/// Outcomes recorded as a job progresses, so a job that is dropped part-way
/// can requeue exactly the submissions that have no outcome yet.
pub(super) struct Report {
    pub outcomes: Vec<Outcome>,
    pub losses: Vec<Loss>,
    pub ready: Vec<usize>,
    /// Batches being uploaded: each row's submissions in batch order, and how
    /// many leading rows have been delivered so far.
    pub uploads: Vec<(BatchSubmissions, Arc<AtomicUsize>)>,
}
impl Report {
    pub fn new(count: usize) -> Self {
        Self {
            outcomes: vec![Outcome::Pending; count],
            losses: Vec::new(),
            ready: Vec::new(),
            uploads: Vec::new(),
        }
    }
    /// Settle rows whose request already succeeded, so a job dropped mid-upload
    /// does not send them again.
    pub fn settle_delivered(&mut self) {
        for (rows, delivered) in std::mem::take(&mut self.uploads) {
            let delivered = delivered.load(Ordering::Acquire);
            for submissions in rows.iter().take(delivered) {
                self.settle(submissions.iter().copied());
            }
        }
    }
    pub fn settle(&mut self, submissions: impl IntoIterator<Item = usize>) {
        for index in submissions {
            if matches!(self.outcomes[index], Outcome::Pending) {
                self.outcomes[index] = Outcome::Delivered;
                self.ready.push(index);
            }
        }
    }
    pub fn failed(&mut self, submissions: Vec<usize>, error: &anyhow::Error, preparation: bool) {
        let error = DeliveryError::from_error(error);
        self.fail_submissions(&submissions, &error);
        self.losses.push(Loss {
            submissions,
            preparation,
        });
    }

    pub fn fail_submissions(&mut self, submissions: &[usize], error: &DeliveryError) {
        for &index in submissions {
            if matches!(self.outcomes[index], Outcome::Pending) {
                self.ready.push(index);
            }
            self.outcomes[index] = Outcome::Failed(error.clone());
        }
    }
}
