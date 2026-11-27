//! Scoped delivery observations shared by client and resource facades.
use crate::{
    BraintrustError, Completion, EnqueueError, ExecutionError, FlushError, FlushFailure,
    FlushReport, WriteError,
};
use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

/// Cumulative counts of rows the log queue discarded instead of uploading.
///
/// Counts only grow over the life of the client, so callers can export them as
/// monotonic counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct DroppedRows {
    /// Rejected because the queue was full or already closed.
    pub queue_full: u64,
    /// Discarded because the row could not be prepared or serialized for upload.
    pub preparation_failed: u64,
    /// Discarded after an upload failed and any retries were exhausted.
    pub upload_failed: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct DeliveryError {
    shared: Arc<crate::DeliveryError>,
}
/// Copy an error for another owner, keeping API status codes inspectable.
pub(crate) fn duplicate_error(error: &anyhow::Error) -> anyhow::Error {
    match error.downcast_ref::<BraintrustError>() {
        Some(BraintrustError::Api { status, message }) => {
            anyhow::Error::from(BraintrustError::Api {
                status: *status,
                message: message.clone(),
            })
        }
        _ => anyhow::anyhow!(error.to_string()),
    }
}
impl DeliveryError {
    pub(crate) fn from_error(error: &anyhow::Error) -> Self {
        Self {
            shared: Arc::new(crate::DeliveryError {
                source: duplicate_error(error),
            }),
        }
    }
    pub fn write_error(&self) -> WriteError {
        WriteError::Delivery(self.shared.clone())
    }
}
pub(crate) type Notification = (
    oneshot::Sender<Result<FlushReport, FlushError>>,
    Result<FlushReport, FlushError>,
);
struct Waiter {
    cutoff: u64,
    delivered: u64,
    failed: u64,
    error: Option<WriteError>,
    sender: oneshot::Sender<Result<FlushReport, FlushError>>,
}
#[derive(Default)]
struct State {
    pending: BTreeSet<u64>,
    waiters: Vec<Waiter>,
    drops: DroppedRows,
    closed: bool,
    terminal: Option<ExecutionError>,
}
#[derive(Default)]
pub(crate) struct Observation {
    state: Mutex<State>,
}
impl Observation {
    pub fn closed(&self) -> bool {
        self.state.lock().unwrap().closed
    }
    fn terminal(&self) -> Option<ExecutionError> {
        self.state.lock().unwrap().terminal
    }
    pub fn close(&self) {
        self.state.lock().unwrap().closed = true;
    }

    pub fn time_out(&self) {
        self.state.lock().unwrap().terminal = Some(ExecutionError::TimedOut);
    }

    pub fn accept(&self, sequence: u64) {
        self.state.lock().unwrap().pending.insert(sequence);
    }
    #[cfg(test)]
    pub fn finish(
        &self,
        sequence: u64,
        error: Option<&DeliveryError>,
        preparation: u64,
        upload: u64,
    ) {
        {
            let mut state = self.state.lock().unwrap();
            state.drops.preparation_failed += preparation;
            state.drops.upload_failed += upload;
        }
        self.finish_write(sequence, error.map(DeliveryError::write_error).as_ref());
    }
    #[cfg(test)]
    pub fn finish_write(&self, sequence: u64, error: Option<&WriteError>) {
        let mut notifications = Vec::new();
        self.retire(sequence, error, &mut notifications);
        for (sender, result) in notifications {
            let _ = sender.send(result);
        }
    }
    pub fn retire(
        &self,
        sequence: u64,
        error: Option<&WriteError>,
        notifications: &mut Vec<Notification>,
    ) {
        let mut state = self.state.lock().unwrap();
        if !state.pending.remove(&sequence) {
            return;
        }
        let waiters = std::mem::take(&mut state.waiters);
        for mut waiter in waiters {
            if waiter.sender.is_closed() {
                continue;
            }
            if sequence <= waiter.cutoff {
                if let Some(error) = error {
                    waiter.failed += 1;
                    if waiter.error.is_none() {
                        waiter.error = Some(error.clone());
                    }
                } else {
                    waiter.delivered += 1;
                }
            }
            if state.pending.range(..=waiter.cutoff).next().is_none() {
                let result = match waiter.error {
                    Some(WriteError::Delivery(cause)) => Err(FlushError::Delivery(FlushFailure {
                        failed: waiter.failed,
                        cause,
                    })),
                    Some(WriteError::Execution(error)) => Err(FlushError::Execution(error)),
                    Some(WriteError::Admission(error)) => Err(FlushError::Admission(error)),
                    None => Ok(FlushReport {
                        delivered: waiter.delivered,
                    }),
                };
                notifications.push((waiter.sender, result));
            } else {
                state.waiters.push(waiter);
            }
        }
        drop(state);
    }
    pub fn record_loss(&self, preparation: bool, visited: &mut HashSet<usize>) {
        if !visited.insert(self as *const Self as usize) {
            return;
        }
        let mut state = self.state.lock().unwrap();
        if preparation {
            state.drops.preparation_failed += 1;
        } else {
            state.drops.upload_failed += 1;
        }
        drop(state);
    }
    pub fn queue_full(&self) {
        self.state.lock().unwrap().drops.queue_full += 1;
    }
    pub fn dropped_rows(&self) -> DroppedRows {
        self.state.lock().unwrap().drops
    }
    pub fn len(&self) -> usize {
        self.state.lock().unwrap().pending.len()
    }
    pub fn barrier(&self, cutoff: u64, allow_closed: bool) -> Completion<FlushReport, FlushError> {
        let (sender, mut completion) = Completion::pair();
        let terminal = self.terminal();
        let closed = self.closed();
        let mut state = self.state.lock().unwrap();
        state.waiters.retain(|w| !w.sender.is_closed());
        if let Some(error) = terminal {
            completion.record_issuance(Err(error.into()));
        } else if closed && !allow_closed {
            completion.record_issuance(Err(EnqueueError::Closed.into()));
        } else if !allow_closed && state.waiters.len() >= 1024 {
            completion.record_issuance(Err(EnqueueError::Full.into()));
        } else if state.pending.range(..=cutoff).next().is_none() {
            let _ = sender.send(Ok(FlushReport::default()));
        } else {
            state.waiters.push(Waiter {
                cutoff,
                delivered: 0,
                failed: 0,
                error: None,
                sender,
            });
        }
        completion
    }
}
