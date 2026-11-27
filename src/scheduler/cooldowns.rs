use super::Scope;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;
use tokio::time::Instant;

const MAX_SCOPES: usize = 256;

/// A pause can apply to an organization or to one destination within it.
#[derive(Default)]
pub(super) struct Cooldowns(Mutex<HashMap<Scope, Instant>>);

impl Cooldowns {
    pub(super) fn deadline(&self, scope: &Scope) -> Option<Instant> {
        let now = Instant::now();
        let mut deadlines = self.0.lock().unwrap();
        deadlines.retain(|_, until| *until > now);
        let scoped = deadlines.get(scope).copied();
        if scope.has_destination() {
            scoped.max(deadlines.get(&scope.org_scope()).copied())
        } else {
            scoped
        }
    }

    pub(super) fn pause(&self, scope: &Scope, delay: Duration) {
        let now = Instant::now();
        let mut deadlines = self.0.lock().unwrap();
        deadlines.retain(|_, until| *until > now);
        if deadlines.len() >= MAX_SCOPES && !deadlines.contains_key(scope) {
            // Make room for the new pause by forgetting the one about to expire.
            if let Some(soonest) = deadlines
                .iter()
                .min_by_key(|(_, until)| **until)
                .map(|(scope, _)| scope.clone())
            {
                deadlines.remove(&soonest);
            }
        }
        let until = now + delay;
        deadlines
            .entry(scope.clone())
            .and_modify(|old| *old = (*old).max(until))
            .or_insert(until);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(org: &str) -> Scope {
        Scope::new("https://example.com", org)
    }

    #[tokio::test]
    async fn destination_cooldown_is_local_and_org_cooldown_covers_every_destination() {
        let cooldowns = Cooldowns::default();
        let org = scope("org");
        let throttled = org.for_destination(Some("throttled".into()));
        let healthy = org.for_destination(Some("healthy".into()));
        cooldowns.pause(&throttled, Duration::from_secs(30));
        assert!(cooldowns.deadline(&throttled).is_some());
        assert!(cooldowns.deadline(&healthy).is_none());
        assert!(cooldowns.deadline(&org).is_none());

        cooldowns.pause(&org, Duration::from_secs(30));
        assert!(cooldowns.deadline(&healthy).is_some());
        assert!(cooldowns.deadline(&org).is_some());
    }

    #[tokio::test]
    async fn a_full_cooldown_table_still_pauses_newly_throttled_orgs() {
        let cooldowns = Cooldowns::default();
        for i in 0..256 {
            cooldowns.pause(&scope(&format!("org{i}")), Duration::from_secs(60 + i));
        }
        let throttled = scope("new");
        cooldowns.pause(&throttled, Duration::from_secs(30));
        assert!(cooldowns.deadline(&throttled).is_some());
        // The pause closest to expiring made room.
        assert!(cooldowns.deadline(&scope("org0")).is_none());
        assert_eq!(cooldowns.0.lock().unwrap().len(), 256);
    }
    #[tokio::test]
    async fn pauses_apply_to_their_org_only() {
        let cooldowns = Cooldowns::default();
        cooldowns.pause(&scope("org"), Duration::from_secs(30));
        assert!(cooldowns.deadline(&scope("org")).is_some());
        assert!(cooldowns.deadline(&scope("other")).is_none());
        assert!(cooldowns
            .deadline(&Scope::new("https://elsewhere.com", "org"))
            .is_none());
    }
}
