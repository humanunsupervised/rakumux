//! Session event fan-out to subscribers (TUIs, CLI `events`, agents).
//!
//! Each subscriber has a bounded queue. A subscriber that falls
//! [`QUEUE_CAPACITY`] events behind is dropped: its receiver disconnects and
//! the client resubscribes (and re-lists) instead of the daemon buffering
//! without bound.
//!
//! Each published event is matched against every subscriber's [`Filter`].
//! A subscriber watching [`Scopes::All`] matches any scope without keeping a
//! list of them, so a scope that gets its first session after it subscribed
//! is watched like any other, at no extra cost per event.

use session_core::api::{EventEnvelope, EventKind, SessionEvent};
use session_core::ids::SessionId;
use session_core::model::ScopeRoot;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::Mutex;

pub(crate) const QUEUE_CAPACITY: usize = 4096;

/// The scopes (projects) a list or subscription covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Scopes {
    /// Every scope, including ones that get their first session later
    /// (`all`; user callers only).
    All,
    /// These scopes; at least one.
    Only(Vec<ScopeRoot>),
}

impl Scopes {
    pub fn contains(&self, scope: &ScopeRoot) -> bool {
        match self {
            Scopes::All => true,
            Scopes::Only(scopes) => scopes.contains(scope),
        }
    }
}

/// What a subscriber wants to see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Filter {
    /// Scopes (projects) watched.
    pub scopes: Scopes,
    /// Empty = every session in those scopes.
    pub session_ids: Vec<SessionId>,
    /// Empty = every kind.
    pub kinds: Vec<EventKind>,
    /// The subscribing session, so views carry `is_self`.
    pub self_id: Option<SessionId>,
    /// Deliver `Reply` events (user callers that asked for them).
    pub replies: bool,
}

impl Filter {
    fn accepts(&self, envelope: &EventEnvelope) -> bool {
        if !self.scopes.contains(&envelope.scope) {
            return false;
        }
        if !self.session_ids.is_empty() && !self.session_ids.contains(&envelope.session_id) {
            return false;
        }
        if matches!(envelope.event, SessionEvent::Reply { .. }) && !self.replies {
            return false;
        }
        match kind_of(&envelope.event) {
            Some(kind) => self.kinds.is_empty() || self.kinds.contains(&kind),
            None => false,
        }
    }
}

struct Subscriber {
    filter: Filter,
    tx: SyncSender<EventEnvelope>,
}

#[derive(Default)]
pub(crate) struct EventBus {
    subscribers: Mutex<Vec<Subscriber>>,
}

impl EventBus {
    pub fn subscribe(&self, filter: Filter) -> Receiver<EventEnvelope> {
        let (tx, rx) = mpsc::sync_channel(QUEUE_CAPACITY);
        self.lock().push(Subscriber { filter, tx });
        rx
    }

    pub fn publish(&self, envelope: EventEnvelope) {
        let mut subscribers = self.lock();
        subscribers.retain(|subscriber| {
            if !subscriber.filter.accepts(&envelope) {
                return true;
            }
            let mut envelope = envelope.clone();
            mark_self(&mut envelope, subscriber.filter.self_id.as_ref());
            match subscriber.tx.try_send(envelope) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) => {
                    crate::log_warn!("control: dropping a subscriber that fell behind");
                    false
                }
                Err(TrySendError::Disconnected(_)) => false,
            }
        });
    }

    /// Disconnect every subscriber (daemon shutdown).
    pub fn close_all(&self) {
        self.lock().clear();
    }

    #[cfg(test)]
    pub fn subscriber_count(&self) -> usize {
        self.lock().len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Subscriber>> {
        // A panic while publishing must not take event delivery down with it.
        self.subscribers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

pub(crate) fn kind_of(event: &SessionEvent) -> Option<EventKind> {
    Some(match event {
        SessionEvent::Created { .. } => EventKind::Created,
        SessionEvent::Updated { .. } => EventKind::Updated,
        SessionEvent::Removed => EventKind::Removed,
        SessionEvent::Exited { .. } => EventKind::Exited,
        SessionEvent::Lifecycle { .. } => EventKind::Lifecycle,
        SessionEvent::Reported { .. } => EventKind::Reported,
        SessionEvent::Activity => EventKind::Activity,
        SessionEvent::Reply { .. } => EventKind::Reply,
        SessionEvent::Unknown => return None,
    })
}

fn mark_self(envelope: &mut EventEnvelope, self_id: Option<&SessionId>) {
    let is_self = self_id == Some(&envelope.session_id);
    match &mut envelope.event {
        SessionEvent::Created { session } | SessionEvent::Updated { session } => {
            session.is_self = is_self;
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(scope: &str, id: &str, event: SessionEvent) -> EventEnvelope {
        EventEnvelope {
            scope: ScopeRoot::from_canonical(scope.into()),
            session_id: SessionId::from_string(id),
            at: 0,
            event,
        }
    }

    fn filter(scope: &str, ids: &[&str], kinds: &[EventKind]) -> Filter {
        Filter {
            scopes: Scopes::Only(vec![ScopeRoot::from_canonical(scope.into())]),
            session_ids: ids.iter().map(|i| SessionId::from_string(*i)).collect(),
            kinds: kinds.to_vec(),
            self_id: None,
            replies: false,
        }
    }

    #[test]
    fn replies_reach_only_subscriptions_that_asked() {
        let bus = EventBus::default();
        let plain = bus.subscribe(filter("/s", &[], &[]));
        let replies = bus.subscribe(Filter {
            scopes: Scopes::All,
            replies: true,
            ..filter("/s", &[], &[])
        });
        let reply = session_core::api::AgentReply {
            id: session_core::ids::ReplyId::generate(),
            session_id: SessionId::from_string("A"),
            agent: "codex".into(),
            conversation_id: None,
            text: "done".into(),
            truncated: false,
            finished_at: 1,
            position: None,
        };
        bus.publish(envelope("/s", "A", SessionEvent::Reply { reply }));
        bus.publish(envelope("/s", "A", SessionEvent::Activity));
        assert_eq!(plain.try_iter().count(), 1, "only the activity");
        assert_eq!(replies.try_iter().count(), 2);
    }

    fn ids(rx: &Receiver<EventEnvelope>) -> Vec<String> {
        rx.try_iter().map(|e| e.session_id.to_string()).collect()
    }

    #[test]
    fn filters_by_scope_ids_and_kinds() {
        let bus = EventBus::default();
        let all = bus.subscribe(filter("/s", &[], &[]));
        let one = bus.subscribe(filter("/s", &["A"], &[]));
        let exits = bus.subscribe(filter("/s", &[], &[EventKind::Exited]));
        let both = bus.subscribe(Filter {
            scopes: Scopes::Only(vec![
                ScopeRoot::from_canonical("/s".into()),
                ScopeRoot::from_canonical("/t".into()),
            ]),
            ..filter("/s", &[], &[])
        });
        bus.publish(envelope("/s", "A", SessionEvent::Activity));
        bus.publish(envelope("/s", "B", SessionEvent::Exited { code: Some(0) }));
        bus.publish(envelope("/t", "C", SessionEvent::Removed));
        bus.publish(envelope("/u", "D", SessionEvent::Removed));
        assert_eq!(all.try_iter().count(), 2);
        assert_eq!(one.try_iter().count(), 1);
        let got: Vec<_> = exits.try_iter().map(|e| e.session_id).collect();
        assert_eq!(got, vec![SessionId::from_string("B")]);
        assert_eq!(ids(&both), ["A", "B", "C"]);
    }

    #[test]
    fn every_scope_includes_scopes_nobody_named() {
        let bus = EventBus::default();
        let everywhere = bus.subscribe(Filter {
            scopes: Scopes::All,
            ..filter("/s", &[], &[])
        });
        let removals = bus.subscribe(Filter {
            scopes: Scopes::All,
            ..filter("/s", &[], &[EventKind::Removed])
        });
        let one = bus.subscribe(Filter {
            scopes: Scopes::All,
            ..filter("/s", &["D"], &[])
        });
        bus.publish(envelope("/s", "A", SessionEvent::Activity));
        bus.publish(envelope("/t", "C", SessionEvent::Removed));
        bus.publish(envelope("/u", "D", SessionEvent::Activity));
        bus.publish(envelope("/u", "D", SessionEvent::Removed));
        assert_eq!(ids(&everywhere), ["A", "C", "D", "D"]);
        assert_eq!(ids(&removals), ["C", "D"]);
        assert_eq!(ids(&one), ["D", "D"]);
    }

    #[test]
    fn dropped_receivers_and_slow_subscribers_are_removed() {
        let bus = EventBus::default();
        let gone = bus.subscribe(filter("/s", &[], &[]));
        drop(gone);
        let slow = bus.subscribe(filter("/s", &[], &[]));
        for _ in 0..=QUEUE_CAPACITY {
            bus.publish(envelope("/s", "A", SessionEvent::Activity));
        }
        assert_eq!(bus.subscriber_count(), 0);
        assert_eq!(slow.try_iter().count(), QUEUE_CAPACITY);
        assert!(slow.recv().is_err());
    }
}
