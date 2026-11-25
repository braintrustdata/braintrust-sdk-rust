//! Request permits rotate across orgs and destinations. Backoff never holds a permit.
use crate::destination::Destination;
use indexmap::IndexMap;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;

/// Who a request counts against. Every SDK request runs inside one, so permits
/// rotate fairly and a 429 pauses only the requests sharing its limit.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) struct Scope {
    pub endpoint: String,
    /// The org ID, or a stand-in for the credential's org when it is unknown.
    pub org: String,
    /// `None` for requests that are not tied to one destination.
    pub destination: Option<Destination>,
}
impl Scope {
    pub fn new(endpoint: impl Into<String>, org: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            org: org.into(),
            destination: None,
        }
    }
    pub fn with_destination(mut self, destination: Destination) -> Self {
        self.destination = Some(destination);
        self
    }
    /// The enclosing scope without its destination, for org-level requests.
    pub fn current_org() -> Option<Self> {
        SCOPE.try_with(Self::org_wide).ok()
    }
    fn org_wide(&self) -> Self {
        Self {
            destination: None,
            ..self.clone()
        }
    }
}
tokio::task_local! { pub(crate) static SCOPE: Scope; }

/// How widely a 429 pauses later requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pause {
    /// Only requests for the same destination wait.
    Destination,
    /// Every request for the same endpoint and org waits.
    Org,
}

struct Request {
    bytes: usize,
    sender: oneshot::Sender<Permit>,
}
#[derive(Default)]
struct DestinationQueue {
    pending: VecDeque<Request>,
    credit: usize,
}
#[derive(Default)]
struct Org {
    destinations: IndexMap<Option<Destination>, DestinationQueue>,
}
#[derive(Default)]
struct State {
    active: usize,
    orgs: IndexMap<Option<(String, String)>, Org>,
}
pub(crate) struct Scheduler {
    max: usize,
    state: Mutex<State>,
    cooldowns: Mutex<HashMap<Scope, tokio::time::Instant>>,
}
pub(crate) struct Permit {
    scheduler: Option<Arc<Scheduler>>,
}
impl Scheduler {
    pub fn new(max: usize) -> Arc<Self> {
        Arc::new(Self {
            max,
            state: Mutex::new(State::default()),
            cooldowns: Mutex::new(HashMap::new()),
        })
    }
    fn deadline(&self, scope: Option<&Scope>) -> Option<tokio::time::Instant> {
        let scope = scope?;
        let mut cooldowns = self.cooldowns.lock().unwrap();
        cooldowns.retain(|_, until| *until > tokio::time::Instant::now());
        let own = cooldowns.get(scope).copied();
        let org = scope
            .destination
            .is_some()
            .then(|| cooldowns.get(&scope.org_wide()).copied())
            .flatten();
        own.max(org)
    }
    pub fn cool_down(&self, delay: Duration, pause: Pause) {
        if let Ok(scope) = SCOPE.try_with(Clone::clone) {
            let scope = match pause {
                Pause::Destination => scope,
                Pause::Org => scope.org_wide(),
            };
            let mut cooldowns = self.cooldowns.lock().unwrap();
            cooldowns.retain(|_, until| *until > tokio::time::Instant::now());
            if cooldowns.len() >= 256 && !cooldowns.contains_key(&scope) {
                // Dropping the new pause would let a throttled destination retry
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
    }
    pub async fn acquire(self: &Arc<Self>, bytes: usize) -> Permit {
        let scope = SCOPE.try_with(Clone::clone).ok();
        loop {
            if let Some(until) = self.deadline(scope.as_ref()) {
                tokio::time::sleep_until(until).await;
                continue;
            }
            let (sender, receiver) = oneshot::channel();
            {
                let mut state = self.state.lock().unwrap();
                let org = scope.as_ref().map(|s| (s.endpoint.clone(), s.org.clone()));
                let destination = scope.as_ref().and_then(|s| s.destination.clone());
                state
                    .orgs
                    .entry(org)
                    .or_default()
                    .destinations
                    .entry(destination)
                    .or_default()
                    .pending
                    .push_back(Request {
                        bytes: bytes.max(1),
                        sender,
                    });
            }
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
    fn dispatch(self: &Arc<Self>) {
        let mut state = self.state.lock().unwrap();
        while state.active < self.max && !state.orgs.is_empty() {
            let (org_key, mut org) = state.orgs.shift_remove_index(0).unwrap();
            let (destination, mut queue) = org.destinations.shift_remove_index(0).unwrap();
            // Deficit rounds account for bytes without making large uploads serial.
            queue.credit = queue.credit.saturating_add(64 * 1024);
            if queue.pending.front().unwrap().bytes > queue.credit {
                org.destinations.insert(destination, queue);
                state.orgs.insert(org_key, org);
                continue;
            }
            let request = queue.pending.pop_front().unwrap();
            queue.credit -= request.bytes;
            let sender = request.sender;
            if !queue.pending.is_empty() {
                org.destinations.insert(destination, queue);
            }
            if !org.destinations.is_empty() {
                state.orgs.insert(org_key, org);
            }
            if sender.is_closed() {
                continue;
            }
            // Reserve before sending. If cancellation races the send, drop outside the lock.
            state.active += 1;
            let permit = Permit {
                scheduler: Some(self.clone()),
            };
            if let Err(mut permit) = sender.send(permit) {
                state.active -= 1;
                permit.scheduler = None;
            }
        }
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        if let Some(scheduler) = self.scheduler.take() {
            {
                scheduler.state.lock().unwrap().active -= 1;
            }
            scheduler.dispatch();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scope(org: &str, destination: &str) -> Scope {
        Scope::new("https://example.com", org).with_destination(Destination::row(
            serde_json::json!({"project_id":destination})
                .as_object()
                .unwrap(),
        ))
    }
    #[tokio::test]
    async fn permits_rotate_orgs_and_destinations_and_cancelled_waits_release_capacity() {
        let scheduler = Scheduler::new(1);
        let initial = scheduler.acquire(1).await;
        let a = SCOPE.scope(scope("a", "a1"), scheduler.acquire(1));
        let another_a = SCOPE.scope(scope("a", "a1"), scheduler.acquire(1));
        let a2 = SCOPE.scope(scope("a", "a2"), scheduler.acquire(1));
        let b = SCOPE.scope(scope("b", "b1"), scheduler.acquire(1));
        tokio::pin!(a, another_a, a2, b);
        // Poll each once to establish a deterministic waiting order.
        for request in [a.as_mut(), another_a.as_mut(), a2.as_mut(), b.as_mut()] {
            assert!(futures::poll!(request).is_pending());
        }
        drop(initial);
        let first = a.await;
        drop(first);
        let second = b.await;
        assert!(futures::poll!(a2.as_mut()).is_pending());
        drop(second);
        // Rotate to the second destination before the first gets another turn.
        drop(a2.await);
        drop(another_a.await);
        assert_eq!(scheduler.state.lock().unwrap().active, 0);
        let initial = scheduler.acquire(1).await;
        let mut cancelled = Box::pin(scheduler.acquire(1));
        assert!(futures::poll!(cancelled.as_mut()).is_pending());
        drop(cancelled);
        drop(initial);
        drop(scheduler.acquire(1).await);
        assert_eq!(scheduler.state.lock().unwrap().active, 0);
        assert!(scheduler.state.lock().unwrap().orgs.is_empty());
    }
    #[tokio::test]
    async fn a_full_cooldown_table_still_pauses_newly_throttled_destinations() {
        let scheduler = Scheduler::new(1);
        for i in 0..256 {
            SCOPE.sync_scope(scope("org", &format!("p{i}")), || {
                scheduler.cool_down(Duration::from_secs(60 + i), Pause::Destination)
            });
        }
        let throttled = scope("org", "new");
        SCOPE.sync_scope(throttled.clone(), || {
            scheduler.cool_down(Duration::from_secs(30), Pause::Destination)
        });
        assert!(scheduler.deadline(Some(&throttled)).is_some());
        // The pause closest to expiring made room.
        assert!(scheduler.deadline(Some(&scope("org", "p0"))).is_none());
        assert_eq!(scheduler.cooldowns.lock().unwrap().len(), 256);
    }
    #[tokio::test]
    async fn pauses_apply_to_their_destination_or_to_the_whole_org() {
        let scheduler = Scheduler::new(1);
        let a = scope("org", "a");
        let b = scope("org", "b");
        let other_org = scope("other", "a");
        SCOPE.sync_scope(a.clone(), || {
            scheduler.cool_down(Duration::from_secs(30), Pause::Destination)
        });
        assert!(scheduler.deadline(Some(&a)).is_some());
        assert!(scheduler.deadline(Some(&b)).is_none());
        SCOPE.sync_scope(b.clone(), || {
            scheduler.cool_down(Duration::from_secs(30), Pause::Org)
        });
        assert!(scheduler.deadline(Some(&b)).is_some());
        assert!(scheduler.deadline(Some(&scope("org", "c"))).is_some());
        assert!(scheduler
            .deadline(Some(&Scope::new("https://example.com", "org")))
            .is_some());
        assert!(scheduler.deadline(Some(&other_org)).is_none());
    }
    #[tokio::test]
    async fn byte_deficits_allow_small_destinations_to_progress_before_large_requests() {
        let scheduler = Scheduler::new(1);
        let held = scheduler.acquire(1).await;
        let large = SCOPE.scope(scope("org", "large"), scheduler.acquire(128 * 1024));
        let small = SCOPE.scope(scope("org", "small"), scheduler.acquire(1));
        tokio::pin!(large, small);
        assert!(futures::poll!(large.as_mut()).is_pending());
        assert!(futures::poll!(small.as_mut()).is_pending());
        drop(held);
        let permit = small.await;
        assert!(futures::poll!(large.as_mut()).is_pending());
        drop(permit);
        drop(large.await);
    }
}
