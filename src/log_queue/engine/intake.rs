use super::partition::{Key, OrgKey};
use crate::log_queue::{
    core::LogQueueCore,
    observation::{Notification, Observation},
    submission::SubmitCommand,
};
use crate::{DeliveryReceipt, WriteError};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::oneshot;

pub(super) struct Entry {
    pub(super) core: Arc<LogQueueCore>,
    pub(super) command: SubmitCommand,
    pub(super) sequence: u64,
    pub(super) row: Option<RowOrderKey>,
    pub(super) route: OrgKey,
}
#[derive(Clone, Eq, Hash, PartialEq)]
pub(super) struct RowOrderKey {
    pub(super) route: OrgKey,
    pub(super) id: String,
}

pub(super) struct ReleasedEntry {
    pub(super) sequence: u64,
    row: Option<RowOrderKey>,
    route: OrgKey,
}

impl Entry {
    pub(super) fn release(self) -> ReleasedEntry {
        ReleasedEntry {
            sequence: self.sequence,
            row: self.row,
            route: self.route,
        }
    }
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
    pub(super) row_order: HashMap<RowOrderKey, RowOrder>,
    pub(super) retained: usize,
    pub(super) org_rows: HashMap<OrgKey, usize>,
}

#[derive(Default)]
pub(super) struct RowOrder {
    pub(super) writes: BTreeMap<u64, Key>,
    by_partition: HashMap<Key, BTreeSet<u64>>,
}

impl RowOrder {
    pub(super) fn insert(&mut self, sequence: u64, key: Key) {
        self.by_partition
            .entry(key.clone())
            .or_default()
            .insert(sequence);
        self.writes.insert(sequence, key);
    }
}

impl Intake {
    pub(super) fn release_row(&mut self, released: ReleasedEntry) -> Vec<Key> {
        let ReleasedEntry {
            row,
            sequence,
            route,
        } = released;
        self.retained -= 1;
        if let Some(rows) = self.org_rows.get_mut(&route) {
            *rows -= 1;
            if *rows == 0 {
                self.org_rows.remove(&route);
            }
        }
        let Some(row) = row else {
            return Vec::new();
        };
        let Some(order) = self.row_order.get_mut(&row) else {
            return Vec::new();
        };
        let Some(released) = order.writes.remove(&sequence) else {
            return Vec::new();
        };
        if order.writes.is_empty() {
            self.row_order.remove(&row);
            return Vec::new();
        }

        let sequences = order
            .by_partition
            .get_mut(&released)
            .expect("registered partition");
        sequences.remove(&sequence);
        if sequences.is_empty() {
            order.by_partition.remove(&released);
        }
        order
            .by_partition
            .iter()
            .filter(|(candidate, sequences)| {
                sequences.first().is_some_and(|first| *first > sequence)
                    && released.aliases(candidate)
            })
            .map(|(candidate, _)| candidate.clone())
            .collect()
    }
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
