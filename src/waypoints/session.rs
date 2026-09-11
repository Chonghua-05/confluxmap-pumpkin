//! Per-connection shared-waypoint state.
//!
//! Everything here is scoped to one connection and dies with it: the negotiated
//! protocol minor, whether the peer subscribed, whether it is an operator right
//! now, and whether it has earned a mute. Mutation idempotency deliberately does
//! *not* live here - it outlives a reconnect, because a client that retries an
//! operation after a dropped connection must not publish the same point twice.
//!
//! # Why a session exists at all
//!
//! Nothing but a `HELLO` is answered before a peer is `compatible`, and only a
//! subscribed peer receives deltas. That is what keeps a client that speaks a
//! different major version from interpreting this server's frames as its own.

use std::collections::HashMap;

use pumpkin_plugin_api::mobs::Uuid;

use crate::identity::Id;

/// Most sessions tracked at once.
pub const MAX_TRACKED_SESSIONS: usize = 2_048;
/// Malformed payloads a connection may send before it is muted.
pub const MAX_MALFORMED_STRIKES: u8 = 3;
/// Control requests a fresh connection may burst.
pub const CONTROL_REQUEST_BURST: f64 = 8.0;
/// Sustained control requests per minute, per connection.
pub const CONTROL_REQUESTS_PER_MINUTE: f64 = 60.0;

/// The identity of one connected player, as the protocol needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    /// Their identity, used as the session key.
    pub id: Id,
    /// Their name, for logs and for the `publisherName` field.
    pub name: String,
    /// Whether they are an operator (permission level two or above).
    pub operator: bool,
}

impl Peer {
    /// The session key for a UUID.
    pub fn key(id: Uuid) -> Id {
        Id::of(id)
    }

    /// Whether an op level counts as an operator, matching the reference
    /// server's `hasPermission(player, 2)`.
    pub fn is_operator(permission_level: u8) -> bool {
        permission_level >= 2
    }
}

/// A token bucket, in milliseconds.
///
/// Control requests - `HELLO`, `SUBSCRIBE` and the mutations - are charged
/// against one bucket per connection. A snapshot can list 512 points, so an
/// unthrottled client could ask for megabytes per second by resubscribing in a
/// loop; the burst is what lets a legitimate client complete a handshake and an
/// initial subscribe back to back.
#[derive(Clone, Debug)]
pub struct TokenBucket {
    capacity: f64,
    refill_per_ms: f64,
    tokens: f64,
    last_ms: i64,
}

impl TokenBucket {
    /// A full bucket of `capacity` tokens, refilling at `per_minute`.
    pub fn new(capacity: f64, per_minute: f64, now_ms: i64) -> Self {
        TokenBucket {
            capacity,
            refill_per_ms: per_minute / 60_000.0,
            tokens: capacity,
            last_ms: now_ms,
        }
    }

    /// Takes one token if there is one.
    pub fn try_consume(&mut self, now_ms: i64) -> bool {
        if now_ms > self.last_ms {
            let elapsed = (now_ms - self.last_ms) as f64;
            self.tokens = self.capacity.min(self.tokens + elapsed * self.refill_per_ms);
            self.last_ms = now_ms;
        }
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}

/// One connection's protocol state.
#[derive(Clone, Debug)]
pub struct Session {
    compatible: bool,
    negotiated_minor: i32,
    subscribed: bool,
    operator: bool,
    malformed_strikes: u8,
    muted: bool,
    control: TokenBucket,
}

impl Session {
    fn new(now_ms: i64) -> Self {
        Session {
            compatible: false,
            negotiated_minor: -1,
            subscribed: false,
            operator: false,
            malformed_strikes: 0,
            muted: false,
            control: TokenBucket::new(
                CONTROL_REQUEST_BURST,
                CONTROL_REQUESTS_PER_MINUTE,
                now_ms,
            ),
        }
    }

    /// Whether the peer completed a `HELLO` this server understood.
    pub fn is_compatible(&self) -> bool {
        self.compatible && !self.muted
    }

    /// Whether the peer asked for the catalog and may receive deltas.
    pub fn is_subscribed(&self) -> bool {
        self.is_compatible() && self.subscribed
    }

    /// Whether the peer has been muted for malformed payloads.
    pub fn is_muted(&self) -> bool {
        self.muted
    }

    /// Whether the peer is an operator, as of the last refresh.
    pub fn is_operator(&self) -> bool {
        self.operator
    }

    /// The negotiated minor, or `0` before a compatible exchange.
    pub fn effective_minor(&self) -> i32 {
        if self.is_compatible() {
            self.negotiated_minor.max(0)
        } else {
            0
        }
    }

    /// Records a live permission change.
    ///
    /// Called on every request the peer makes, so a client does not have to wait
    /// for the periodic refresh to be told it has been granted op.
    pub fn set_operator(&mut self, operator: bool) {
        self.operator = operator;
    }

    /// Marks the peer as receiving the catalog and its deltas.
    ///
    /// A disable clears it explicitly: re-enabling must be a fresh `SUBSCRIBE`,
    /// because the catalog may have moved on while the peer was unsubscribed.
    pub fn set_subscribed(&mut self, subscribed: bool) {
        self.subscribed = subscribed;
    }

    /// Charges one control request against this connection's budget.
    pub fn charge_control(&mut self, now_ms: i64) -> bool {
        self.control.try_consume(now_ms)
    }

    /// Records a malformed payload, returning the outcome to log.
    pub fn record_malformed(&mut self) -> MalformedOutcome {
        let was_muted = self.muted;
        self.malformed_strikes = self
            .malformed_strikes
            .saturating_add(1)
            .min(MAX_MALFORMED_STRIKES);
        self.muted = self.malformed_strikes >= MAX_MALFORMED_STRIKES;
        MalformedOutcome {
            strikes: self.malformed_strikes,
            newly_muted: !was_muted && self.muted,
        }
    }

    /// Applies a `HELLO`.
    ///
    /// A peer that speaks a different major is left incompatible rather than
    /// disconnected: the client will time out into its own unsupported state, and
    /// everything else on that connection stays silent.
    fn accept_hello(&mut self, major: i32, minor: i32, operator: bool) {
        self.subscribed = false;
        self.compatible = major == crate::waypoints::proto::PROTO_MAJOR && minor >= 0;
        self.negotiated_minor = if minor < 0 {
            0
        } else {
            minor.min(crate::waypoints::proto::PROTO_MINOR)
        };
        self.operator = operator;
    }
}

/// What [`Session::record_malformed`] produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MalformedOutcome {
    /// Strikes accumulated so far.
    pub strikes: u8,
    /// Whether this payload is the one that muted the connection.
    pub newly_muted: bool,
}

/// Every live session, keyed by player UUID.
#[derive(Clone, Debug, Default)]
pub struct Sessions {
    map: HashMap<Id, Session>,
}

impl Sessions {
    /// No sessions.
    pub fn new() -> Self {
        Sessions {
            map: HashMap::new(),
        }
    }

    /// How many sessions are tracked.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// The session for a peer, creating one if there is room.
    ///
    /// `None` means the cap is reached and this peer has no session: an
    /// unbounded map keyed by player UUID is a slow leak, and refusing a new
    /// key is the safe answer.
    pub fn ensure(&mut self, id: Id, now_ms: i64) -> Option<&mut Session> {
        if !self.map.contains_key(&id) && self.map.len() >= MAX_TRACKED_SESSIONS {
            return None;
        }
        Some(self.map.entry(id).or_insert_with(|| Session::new(now_ms)))
    }

    /// The session for a peer, if it has one.
    pub fn get(&self, id: Id) -> Option<&Session> {
        self.map.get(&id)
    }

    /// The session for a peer, mutably.
    pub fn get_mut(&mut self, id: Id) -> Option<&mut Session> {
        self.map.get_mut(&id)
    }

    /// Drops a peer's session.
    pub fn remove(&mut self, id: Id) {
        self.map.remove(&id);
    }

    /// Drops every session.
    pub fn clear(&mut self) {
        self.map.clear();
    }

    /// Marks every subscription stale, so the next enable requires a fresh one.
    pub fn clear_subscriptions(&mut self) {
        for session in self.map.values_mut() {
            session.subscribed = false;
        }
    }

    /// The keys of every peer that should receive deltas, oldest first is not
    /// guaranteed: order is irrelevant to a broadcast.
    pub fn subscribed(&self) -> Vec<Id> {
        self.map
            .iter()
            .filter(|(_, session)| session.is_subscribed())
            .map(|(id, _)| *id)
            .collect()
    }

    /// The keys of every peer that completed a compatible handshake.
    pub fn compatible(&self) -> Vec<Id> {
        self.map
            .iter()
            .filter(|(_, session)| session.is_compatible())
            .map(|(id, _)| *id)
            .collect()
    }

    /// Applies a `HELLO` to a peer's session.
    pub fn hello(
        &mut self,
        id: Id,
        major: i32,
        minor: i32,
        operator: bool,
        now_ms: i64,
    ) -> Option<i32> {
        let session = self.ensure(id, now_ms)?;
        if session.is_muted() || !session.charge_control(now_ms) {
            return None;
        }
        session.accept_hello(major, minor, operator);
        Some(session.effective_minor())
    }

    /// Records a live permission change, returning whether it moved.
    pub fn refresh_operator(&mut self, id: Id, operator: bool) -> bool {
        match self.map.get_mut(&id) {
            Some(session) if session.is_compatible() && session.operator != operator => {
                session.operator = operator;
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bucket_allows_the_burst_then_refills() {
        let mut bucket = TokenBucket::new(3.0, 60.0, 0);
        assert!(bucket.try_consume(0));
        assert!(bucket.try_consume(0));
        assert!(bucket.try_consume(0));
        assert!(!bucket.try_consume(0), "the burst is spent");
        // Sixty a minute is one per second.
        assert!(!bucket.try_consume(500));
        assert!(bucket.try_consume(1_000));
        // ... and never above the capacity.
        let mut idle = TokenBucket::new(2.0, 60.0, 0);
        assert!(idle.try_consume(600_000));
        assert!(idle.try_consume(600_000));
        assert!(!idle.try_consume(600_000));
    }

    #[test]
    fn nothing_is_answered_before_a_hello() {
        let mut sessions = Sessions::new();
        let session = sessions.ensure(Id { high: 1, low: 2 }, 0).expect("room");
        assert!(!session.is_compatible());
        assert!(!session.is_subscribed());
        assert_eq!(session.effective_minor(), 0, "the legacy shape");
    }

    #[test]
    fn a_hello_negotiates_the_lower_minor() {
        let mut sessions = Sessions::new();
        assert_eq!(sessions.hello(Id { high: 1, low: 2 }, 1, 3, true, 0), Some(3));
        let session = sessions.get(Id { high: 1, low: 2 }).expect("exists");
        assert!(session.is_compatible());
        assert!(session.is_operator());
        assert!(!session.is_subscribed(), "a hello clears any subscription");

        // A client ahead of this build is capped, not rejected.
        assert_eq!(sessions.hello(Id { high: 3, low: 4 }, 1, 99, false, 0), Some(3));
        // A negative minor is the pre-negotiation shape.
        assert_eq!(sessions.hello(Id { high: 5, low: 6 }, 1, -1, false, 0), Some(0));
    }

    #[test]
    fn a_foreign_major_stays_silent() {
        let mut sessions = Sessions::new();
        assert_eq!(
            sessions.hello(Id { high: 1, low: 2 }, 7, 0, true, 0),
            Some(0),
            "a foreign major still gets a status saying so"
        );
        let session = sessions.get_mut(Id { high: 1, low: 2 }).expect("a session is still tracked");
        assert!(!session.is_compatible());
        assert!(!session.is_subscribed());
        // ... and it cannot subscribe its way in.
        assert!(session.record_malformed().strikes == 1);
    }

    #[test]
    fn three_malformed_payloads_mute_a_connection() {
        let mut sessions = Sessions::new();
        let session = sessions.ensure(Id { high: 1, low: 2 }, 0).expect("room");
        assert_eq!(
            session.record_malformed(),
            MalformedOutcome {
                strikes: 1,
                newly_muted: false
            }
        );
        assert!(!session.is_muted());
        session.record_malformed();
        assert_eq!(
            session.record_malformed(),
            MalformedOutcome {
                strikes: 3,
                newly_muted: true
            }
        );
        assert!(session.is_muted());
        assert!(!session.is_compatible(), "a muted peer answers nothing");
        // Further strikes do not overflow the counter.
        assert_eq!(session.record_malformed().strikes, MAX_MALFORMED_STRIKES);
    }

    #[test]
    fn a_hello_storm_runs_out_of_control_budget() {
        let mut sessions = Sessions::new();
        let mut answered = 0;
        // At the same instant the bucket is the burst and nothing more.
        for _ in 0..20 {
            if sessions.hello(Id { high: 1, low: 2 }, 1, 3, false, 1_000).is_some() {
                answered += 1;
            }
        }
        assert_eq!(answered, CONTROL_REQUEST_BURST as i32);
    }

    #[test]
    fn subscriptions_are_per_connection_and_clearable() {
        let mut sessions = Sessions::new();
        sessions.hello(Id { high: 1, low: 2 }, 1, 3, false, 0).expect("hello");
        sessions.hello(Id { high: 3, low: 4 }, 1, 3, false, 0).expect("hello");
        sessions.get_mut(Id { high: 1, low: 2 }).expect("exists").subscribed = true;
        assert_eq!(sessions.subscribed(), vec![Id { high: 1, low: 2 }]);
        assert_eq!(sessions.compatible().len(), 2);

        sessions.clear_subscriptions();
        assert!(sessions.subscribed().is_empty());

        sessions.remove(Id { high: 1, low: 2 });
        assert_eq!(sessions.len(), 1);
        sessions.clear();
        assert_eq!(sessions.len(), 0);
    }

    #[test]
    fn an_operator_change_is_reported_once() {
        let mut sessions = Sessions::new();
        sessions.hello(Id { high: 1, low: 2 }, 1, 3, false, 0).expect("hello");
        assert!(sessions.refresh_operator(Id { high: 1, low: 2 }, true), "gained op");
        assert!(!sessions.refresh_operator(Id { high: 1, low: 2 }, true), "no change");
        assert!(sessions.refresh_operator(Id { high: 1, low: 2 }, false), "lost op");
        assert!(!sessions.refresh_operator(Id { high: 9, low: 9 }, true), "unknown peer");
    }

    #[test]
    fn the_session_cap_refuses_new_keys_instead_of_growing() {
        let mut sessions = Sessions::new();
        for index in 0..MAX_TRACKED_SESSIONS {
            assert!(sessions.ensure(Id { high: 0, low: index as u64 }, 0).is_some());
        }
        assert!(
            sessions.ensure(Id { high: 1, low: 1 }, 0).is_none(),
            "the cap holds at {MAX_TRACKED_SESSIONS}"
        );
        // An existing peer is still served.
        assert!(sessions.ensure(Id { high: 0, low: 0 }, 0).is_some());
    }

    #[test]
    fn op_levels_two_and_above_are_operators() {
        assert!(!Peer::is_operator(0));
        assert!(!Peer::is_operator(1));
        assert!(Peer::is_operator(2));
        assert!(Peer::is_operator(4));
    }
}
