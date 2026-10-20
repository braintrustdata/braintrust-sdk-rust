//! Bounded response-task admission, completion publication and cancellation.
use crate::{EnqueueError, ExecutionError};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
};
use tokio::{
    sync::{oneshot, Notify},
    task::AbortHandle,
};

pub(crate) const MAX_RESPONSE_TASKS: usize = 1024;
trait CancelResponse: Send + Sync {
    fn cancel(&self, error: ExecutionError);
}
struct Entry {
    response: Arc<dyn CancelResponse>,
    abort: Option<AbortHandle>,
}
pub(crate) struct Response<T, E>(Mutex<Option<oneshot::Sender<Result<T, E>>>>);
impl<T, E> Response<T, E> {
    fn send(&self, result: Result<T, E>) -> Option<Result<T, E>> {
        let sender = lock(&self.0).take();
        match sender {
            Some(sender) => sender.send(result).err(),
            None => Some(result),
        }
    }
}
impl<T: Send, E: From<ExecutionError> + Send> CancelResponse for Response<T, E> {
    fn cancel(&self, error: ExecutionError) {
        drop(self.send(Err(error.into())));
    }
}
struct State {
    accepting: bool,
    terminal: Option<ExecutionError>,
    next_id: u64,
    active: HashMap<u64, Entry>,
    publishing: usize,
}
pub(crate) struct ResponseTasks {
    state: Mutex<State>,
    changed: Notify,
}
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
impl Default for ResponseTasks {
    fn default() -> Self {
        Self {
            state: Mutex::new(State {
                accepting: true,
                terminal: None,
                next_id: 0,
                active: HashMap::new(),
                publishing: 0,
            }),
            changed: Notify::new(),
        }
    }
}
impl ResponseTasks {
    pub fn admit<T: Send + 'static, E: From<ExecutionError> + Send + 'static>(
        self: &Arc<Self>,
        sender: oneshot::Sender<Result<T, E>>,
    ) -> Result<(TaskGuard, Arc<Response<T, E>>), EnqueueError> {
        let mut state = lock(&self.state);
        if !state.accepting {
            return Err(EnqueueError::Closed);
        }
        if state.active.len() + state.publishing >= MAX_RESPONSE_TASKS {
            return Err(EnqueueError::Full);
        }
        let id = state.next_id;
        state.next_id = id.checked_add(1).ok_or(EnqueueError::Full)?;
        let response = Arc::new(Response(Mutex::new(Some(sender))));
        state.active.insert(
            id,
            Entry {
                response: response.clone(),
                abort: None,
            },
        );
        Ok((
            TaskGuard {
                tasks: self.clone(),
                id,
                finished: false,
            },
            response,
        ))
    }
    pub fn install_abort(&self, id: u64, abort: AbortHandle) {
        let mut state = lock(&self.state);
        if let Some(entry) = state.active.get_mut(&id) {
            entry.abort = Some(abort);
        } else {
            abort.abort();
        } // Completion/cancellation can win the spawn race.
    }
    pub fn close(&self) {
        lock(&self.state).accepting = false;
    }
    pub async fn drained(&self) -> Result<(), ExecutionError> {
        loop {
            let changed = self.changed.notified();
            {
                let state = lock(&self.state);
                if let Some(error) = state.terminal {
                    return Err(error);
                }
                if state.active.is_empty() && state.publishing == 0 {
                    return Ok(());
                }
            }
            changed.await;
        }
    }
    pub fn cancel(&self, error: ExecutionError) {
        let entries = {
            let mut state = lock(&self.state);
            state.accepting = false;
            state.terminal.get_or_insert(error);
            std::mem::take(&mut state.active)
        };
        for entry in entries.into_values() {
            entry.response.cancel(error);
            if let Some(abort) = entry.abort {
                abort.abort();
            }
        }
        self.changed.notify_waiters();
    }
    // Reserve completion atomically against cancellation, then publish outside
    // the lock. Drain still counts the reservation until publication is done.
    fn completing(self: &Arc<Self>, id: u64) -> Option<(Entry, Publication)> {
        let mut state = lock(&self.state);
        let entry = state.active.remove(&id)?;
        state.publishing += 1;
        Some((entry, Publication(self.clone())))
    }
    fn lost(self: &Arc<Self>, id: u64) {
        if let Some((entry, _publication)) = self.completing(id) {
            entry.response.cancel(ExecutionError::WorkerLost);
        }
    }
    #[cfg(test)]
    pub fn outstanding(&self) -> usize {
        let state = lock(&self.state);
        state.active.len() + state.publishing
    }
}
struct Publication(Arc<ResponseTasks>);
impl Drop for Publication {
    fn drop(&mut self) {
        lock(&self.0.state).publishing -= 1;
        self.0.changed.notify_waiters();
    }
}
/// Constructed before spawning so runtime loss before first poll is observable.
pub(crate) struct TaskGuard {
    tasks: Arc<ResponseTasks>,
    pub id: u64,
    finished: bool,
}
impl TaskGuard {
    pub fn complete<T, E>(mut self, response: &Response<T, E>, result: Result<T, E>) {
        self.finished = true;
        if let Some((entry, publication)) = self.tasks.completing(self.id) {
            let unused = response.send(result);
            drop(entry);
            // Neither waking an observer nor dropping its unobserved value runs
            // under the registry lock; both can reenter SDK scheduling.
            drop(unused);
            drop(publication);
        } else {
            drop(result); // Cancellation already published this task's outcome.
        }
    }
}
impl Drop for TaskGuard {
    fn drop(&mut self) {
        if !self.finished {
            self.tasks.lost(self.id);
        }
    }
}
