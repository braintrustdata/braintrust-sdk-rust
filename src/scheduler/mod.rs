//! Request permits rotate across orgs. Backoff never holds a permit.
use indexmap::IndexMap;
use std::collections::VecDeque;
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::{oneshot, OwnedSemaphorePermit, Semaphore};

mod cooldowns;
mod scope;

use cooldowns::Cooldowns;
pub(crate) use scope::{Scope, SCOPE};

pub(crate) fn shared() -> &'static Arc<Scheduler> {
    static SCHEDULER: OnceLock<Arc<Scheduler>> = OnceLock::new();
    SCHEDULER.get_or_init(|| Scheduler::new(crate::service::config().max_concurrent_requests))
}

/// Admit one operation whose error is handled by the caller.
pub(crate) async fn admitted<T>(scope: Scope, operation: impl Future<Output = T>) -> T {
    SCOPE
        .scope(scope, async {
            let _permit = shared().acquire().await;
            operation.await
        })
        .await
}

type OrgQueues = IndexMap<Option<Scope>, VecDeque<oneshot::Sender<Permit>>>;

pub(crate) struct Scheduler {
    slots: Arc<Semaphore>,
    orgs: Mutex<OrgQueues>,
    cooldowns: Cooldowns,
}

pub(crate) struct Permit {
    slot: Option<OwnedSemaphorePermit>,
    scheduler: Arc<Scheduler>,
}

impl Scheduler {
    pub fn new(max: usize) -> Arc<Self> {
        Arc::new(Self {
            slots: Arc::new(Semaphore::new(max)),
            orgs: Mutex::new(IndexMap::new()),
            cooldowns: Cooldowns::default(),
        })
    }
    #[cfg(test)]
    pub fn paused(&self, scope: &Scope) -> bool {
        self.cooldowns.deadline(scope).is_some()
    }

    pub fn cool_down(&self, delay: Duration) {
        let _ = SCOPE.try_with(|scope| self.cooldowns.pause(scope, delay));
    }

    pub async fn acquire(self: &Arc<Self>) -> Permit {
        let scope = SCOPE.try_with(Clone::clone).ok();
        loop {
            if let Some(until) = scope
                .as_ref()
                .and_then(|scope| self.cooldowns.deadline(scope))
            {
                tokio::time::sleep_until(until).await;
                continue;
            }
            let (sender, receiver) = oneshot::channel();
            self.orgs
                .lock()
                .unwrap()
                .entry(scope.as_ref().map(Scope::org_scope))
                .or_default()
                .push_back(sender);
            self.dispatch();
            let permit = receiver.await.expect("scheduler retains queued requests");
            // A 429 may have arrived while this request waited for a permit.
            if scope
                .as_ref()
                .and_then(|scope| self.cooldowns.deadline(scope))
                .is_some()
            {
                drop(permit);
                continue;
            }
            return permit;
        }
    }
    fn next(&self) -> Option<(oneshot::Sender<Permit>, OwnedSemaphorePermit)> {
        let mut orgs = self.orgs.lock().unwrap();
        let slot = self.slots.clone().try_acquire_owned().ok()?;
        while let Some((org, mut queue)) = orgs.shift_remove_index(0) {
            let Some(sender) = queue.pop_front() else {
                continue;
            };
            if !queue.is_empty() {
                orgs.insert(org, queue);
            }
            if !sender.is_closed() {
                return Some((sender, slot));
            }
        }
        None
    }

    fn dispatch(self: &Arc<Self>) {
        while let Some((sender, slot)) = self.next() {
            let permit = Permit {
                slot: Some(slot),
                scheduler: self.clone(),
            };
            if let Err(mut permit) = sender.send(permit) {
                // Release here instead of recursively dispatching from Drop.
                permit.slot.take();
            }
        }
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        if let Some(slot) = self.slot.take() {
            drop(slot);
            self.scheduler.dispatch();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scope(org: &str) -> Scope {
        Scope::new("https://example.com", org)
    }
    #[tokio::test]
    async fn permits_rotate_orgs_and_cancelled_waits_release_capacity() {
        let scheduler = Scheduler::new(1);
        let initial = scheduler.acquire().await;
        let a = SCOPE.scope(scope("a"), scheduler.acquire());
        let another_a = SCOPE.scope(scope("a"), scheduler.acquire());
        let b = SCOPE.scope(scope("b"), scheduler.acquire());
        tokio::pin!(a, another_a, b);
        // Poll each once to establish a deterministic waiting order.
        for request in [a.as_mut(), another_a.as_mut(), b.as_mut()] {
            assert!(futures::poll!(request).is_pending());
        }
        drop(initial);
        let first = a.await;
        drop(first);
        // The other org gets a turn before org a's second request.
        let second = b.await;
        assert!(futures::poll!(another_a.as_mut()).is_pending());
        drop(second);
        drop(another_a.await);
        assert_eq!(scheduler.slots.available_permits(), 1);
        let initial = scheduler.acquire().await;
        let mut cancelled = Box::pin(scheduler.acquire());
        assert!(futures::poll!(cancelled.as_mut()).is_pending());
        drop(cancelled);
        drop(initial);
        drop(scheduler.acquire().await);
        assert_eq!(scheduler.slots.available_permits(), 1);
        assert!(scheduler.orgs.lock().unwrap().is_empty());
    }
}
