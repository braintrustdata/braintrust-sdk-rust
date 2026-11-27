use super::partition::Key;
use crate::log_queue::{
    core::LogQueueCore,
    observation::{Notification, Observation},
    submission::SubmitCommand,
};
use crate::{DeliveryReceipt, WriteError};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::oneshot;

pub(super) struct Entry {
    pub(super) core: Arc<LogQueueCore>,
    pub(super) command: SubmitCommand,
    pub(super) sequence: u64,
}
pub(super) type WriteSender = oneshot::Sender<Result<DeliveryReceipt, WriteError>>;
pub(super) struct Record {
    pub(super) observer: Arc<Observation>,
    pub(super) sender: Option<WriteSender>,
}
#[derive(Default)]
pub(super) struct Intake {
    pub(super) sequence: u64,
    pub(super) records: HashMap<u64, Record>,
    pub(super) partitions: HashMap<Key, Partition>,
}

#[derive(Default)]
pub(super) struct Partition {
    pub(super) pending: VecDeque<Entry>,
    pub(super) running: bool,
}

/// Complete writes under the intake lock, then notify callers after unlocking.
#[derive(Default)]
pub(super) struct FinishedWrites {
    pub(super) flushes: Vec<Notification>,
    pub(super) writes: Vec<(WriteSender, Result<DeliveryReceipt, WriteError>)>,
}

impl FinishedWrites {
    pub(super) fn retire(&mut self, intake: &mut Intake, sequence: u64, error: Option<WriteError>) {
        let Some(record) = intake.records.remove(&sequence) else {
            return;
        };
        record
            .observer
            .retire(sequence, error.as_ref(), &mut self.flushes);
        if let Some(sender) = record.sender {
            self.writes
                .push((sender, error.map_or(Ok(DeliveryReceipt::new()), Err)));
        }
    }

    pub(super) fn notify(self) {
        for (sender, result) in self.flushes {
            let _ = sender.send(result);
        }
        for (sender, result) in self.writes {
            let _ = sender.send(result);
        }
    }
}
