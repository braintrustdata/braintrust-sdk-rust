//! Request permits rotate across orgs. Backoff never holds a permit.
use indexmap::IndexMap;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;

/// Who a request counts against. Every SDK request runs inside one, so permits
/// rotate fairly across orgs and a 429 pauses only the requests sharing its org.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) struct Scope {
    pub endpoint: String,
    /// The org's identity; see [`crate::resolution::OrgIdentity::scope_key`].
    pub org: String,
}
impl Scope {
    pub fn new(endpoint: impl Into<String>, org: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            org: org.into(),
        }
    }
}
tokio::task_local! { pub(crate) static SCOPE: Scope; }

struct Request {
    bytes: usize,
    sender: oneshot::Sender<Permit>,
}
#[derive(Default)]
struct OrgQueue {
    pending: VecDeque<Request>,
    credit: usize,
}
#[derive(Default)]
struct State {
    active: usize,
    orgs: IndexMap<Option<Scope>, OrgQueue>,
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
        cooldowns.get(scope).copied()
    }
    #[cfg(test)]
    pub fn paused(&self, scope: &Scope) -> bool {
        self.deadline(Some(scope)).is_some()
    }
    /// Pause every request in the current scope's org. A 429 does not say
    /// whether a project or an org limit was hit, so the whole org waits.
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
    pub async fn acquire(self: &Arc<Self>, bytes: usize) -> Permit {
        let scope = SCOPE.try_with(Clone::clone).ok();
        loop {
            if let Some(until) = self.deadline(scope.as_ref()) {
                tokio::time::sleep_until(until).await;
                continue;
            }
            let (sender, receiver) = oneshot::channel();
            self.state
                .lock()
                .unwrap()
                .orgs
                .entry(scope.clone())
                .or_default()
                .pending
                .push_back(Request {
                    bytes: bytes.max(1),
                    sender,
                });
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
            let (org, mut queue) = state.orgs.shift_remove_index(0).unwrap();
            // Deficit rounds account for bytes without making large uploads serial.
            queue.credit = queue.credit.saturating_add(64 * 1024);
            if queue.pending.front().unwrap().bytes > queue.credit {
                state.orgs.insert(org, queue);
                continue;
            }
            let request = queue.pending.pop_front().unwrap();
            queue.credit -= request.bytes;
            if !queue.pending.is_empty() {
                state.orgs.insert(org, queue);
            }
            if request.sender.is_closed() {
                continue;
            }
            // Reserve before sending. If cancellation races the send, drop outside the lock.
            state.active += 1;
            let permit = Permit {
                scheduler: Some(self.clone()),
            };
            if let Err(mut permit) = request.sender.send(permit) {
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
    fn scope(org: &str) -> Scope {
        Scope::new("https://example.com", org)
    }
    #[tokio::test]
    async fn permits_rotate_orgs_and_cancelled_waits_release_capacity() {
        let scheduler = Scheduler::new(1);
        let initial = scheduler.acquire(1).await;
        let a = SCOPE.scope(scope("a"), scheduler.acquire(1));
        let another_a = SCOPE.scope(scope("a"), scheduler.acquire(1));
        let b = SCOPE.scope(scope("b"), scheduler.acquire(1));
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
    #[tokio::test]
    async fn byte_deficits_allow_small_orgs_to_progress_before_large_requests() {
        let scheduler = Scheduler::new(1);
        let held = scheduler.acquire(1).await;
        let large = SCOPE.scope(scope("large"), scheduler.acquire(128 * 1024));
        let small = SCOPE.scope(scope("small"), scheduler.acquire(1));
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
