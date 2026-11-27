//! Request permits rotate across orgs. Backoff never holds a permit.
use indexmap::IndexMap;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{oneshot, OwnedSemaphorePermit, Semaphore};

/// Who a request counts against. Every SDK request runs inside one, so permits
/// rotate fairly across orgs and cooldowns can cover an org or a destination.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) struct Scope {
    pub endpoint: String,
    /// The org's identity; see [`crate::resolution::OrgIdentity::scope_key`].
    pub org: String,
    destination: Option<String>,
}
impl Scope {
    pub fn new(endpoint: impl Into<String>, org: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            org: org.into(),
            destination: None,
        }
    }

    pub fn for_destination(&self, destination: Option<String>) -> Self {
        Self {
            destination,
            ..self.clone()
        }
    }

    fn org_scope(&self) -> Self {
        self.for_destination(None)
    }
}
tokio::task_local! { pub(crate) static SCOPE: Scope; }

type OrgQueues = IndexMap<Option<Scope>, VecDeque<oneshot::Sender<Permit>>>;

pub(crate) struct Scheduler {
    slots: Arc<Semaphore>,
    orgs: Mutex<OrgQueues>,
    cooldowns: Mutex<HashMap<Scope, tokio::time::Instant>>,
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
            cooldowns: Mutex::new(HashMap::new()),
        })
    }
    fn deadline(&self, scope: Option<&Scope>) -> Option<tokio::time::Instant> {
        let scope = scope?;
        let mut cooldowns = self.cooldowns.lock().unwrap();
        cooldowns.retain(|_, until| *until > tokio::time::Instant::now());
        let scoped = cooldowns.get(scope).copied();
        if scope.destination.is_some() {
            scoped.max(cooldowns.get(&scope.org_scope()).copied())
        } else {
            scoped
        }
    }
    #[cfg(test)]
    pub fn paused(&self, scope: &Scope) -> bool {
        self.deadline(Some(scope)).is_some()
    }
    /// Pause the request's destination when it has one. Requests without a
    /// destination pause the organization.
    pub fn cool_down(&self, delay: Duration) {
        let Ok(scope) = SCOPE.try_with(Clone::clone) else {
            return;
        };
        let mut cooldowns = self.cooldowns.lock().unwrap();
        cooldowns.retain(|_, until| *until > tokio::time::Instant::now());
        if cooldowns.len() >= 256 && !cooldowns.contains_key(&scope) {
            // Dropping the new pause would let a throttled org retry
            // immediately; forget the pause closest to expiring instead.
            if let Some(soonest) = cooldowns
                .iter()
                .min_by_key(|(_, until)| **until)
                .map(|(key, _)| key.clone())
            {
                cooldowns.remove(&soonest);
            }
        }
        let until = tokio::time::Instant::now() + delay;
        cooldowns
            .entry(scope)
            .and_modify(|old| *old = (*old).max(until))
            .or_insert(until);
    }
    pub async fn acquire(self: &Arc<Self>) -> Permit {
        let scope = SCOPE.try_with(Clone::clone).ok();
        loop {
            if let Some(until) = self.deadline(scope.as_ref()) {
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
            if self.deadline(scope.as_ref()).is_some() {
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
    #[tokio::test]
    async fn destination_cooldown_is_local_and_org_cooldown_covers_every_destination() {
        let scheduler = Scheduler::new(1);
        let org = scope("org");
        let throttled = org.for_destination(Some("throttled".into()));
        let healthy = org.for_destination(Some("healthy".into()));
        SCOPE
            .scope(throttled.clone(), async {
                scheduler.cool_down(Duration::from_secs(30));
            })
            .await;
        assert!(scheduler.paused(&throttled));
        assert!(!scheduler.paused(&healthy));
        assert!(!scheduler.paused(&org));

        SCOPE
            .scope(org.clone(), async {
                scheduler.cool_down(Duration::from_secs(30));
            })
            .await;
        assert!(scheduler.paused(&healthy));
        assert!(scheduler.paused(&org));
    }

    #[tokio::test]
    async fn a_full_cooldown_table_still_pauses_newly_throttled_orgs() {
        let scheduler = Scheduler::new(1);
        for i in 0..256 {
            SCOPE.sync_scope(scope(&format!("org{i}")), || {
                scheduler.cool_down(Duration::from_secs(60 + i))
            });
        }
        let throttled = scope("new");
        SCOPE.sync_scope(throttled.clone(), || {
            scheduler.cool_down(Duration::from_secs(30))
        });
        assert!(scheduler.paused(&throttled));
        // The pause closest to expiring made room.
        assert!(!scheduler.paused(&scope("org0")));
        assert_eq!(scheduler.cooldowns.lock().unwrap().len(), 256);
    }
    #[tokio::test]
    async fn pauses_apply_to_their_org_only() {
        let scheduler = Scheduler::new(1);
        SCOPE.sync_scope(scope("org"), || {
            scheduler.cool_down(Duration::from_secs(30))
        });
        assert!(scheduler.paused(&scope("org")));
        assert!(!scheduler.paused(&scope("other")));
        assert!(!scheduler.paused(&Scope::new("https://elsewhere.com", "org")));
    }
}
