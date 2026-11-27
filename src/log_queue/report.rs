use super::observation::DeliveryError;

pub(super) struct Loss {
    pub submissions: Vec<usize>,
    pub preparation: bool,
}

#[derive(Clone)]
pub(super) enum Outcome {
    Pending,
    Delivered,
    Failed(DeliveryError),
}

/// Outcomes and physical row losses from a delivery job.
pub(super) struct Report {
    pub outcomes: Vec<Outcome>,
    pub losses: Vec<Loss>,
}
impl Report {
    pub fn new(count: usize) -> Self {
        Self {
            outcomes: vec![Outcome::Pending; count],
            losses: Vec::new(),
        }
    }
    pub fn settle(&mut self, submissions: impl IntoIterator<Item = usize>) {
        for index in submissions {
            if matches!(self.outcomes[index], Outcome::Pending) {
                self.outcomes[index] = Outcome::Delivered;
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
            self.outcomes[index] = Outcome::Failed(error.clone());
        }
    }
}
