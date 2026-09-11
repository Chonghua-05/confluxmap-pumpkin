//! Server-authoritative shared-waypoint mutation service.
//!
//! Every field of a request is untrusted. This layer owns the identifiers,
//! publisher metadata, timestamps, revisions, permissions, quotas, rate
//! limiting, idempotency and the durable commit; a client only ever proposes a
//! body. It is the counterpart of the reference companion's
//! `SharedWaypointService`, and the order of its checks is protocol semantics:
//! permission, rate limit, revision and quota are evaluated in the order the
//! reference does, so the error a client sees for a given request is the same.
//!
//! # Why applied idempotency results outlive a connection
//!
//! A client correlates a mutation by `operation_id` and retries it after a
//! dropped connection because it never learned whether the first attempt
//! landed. If the result were discarded with the socket, the retry would create
//! a second point. Results therefore live here, keyed by player and operation,
//! and are only retained for *applied* mutations: replaying a transient refusal
//! such as `RATE_LIMITED` or `PERSISTENCE_FAILED` forever would be worse than
//! re-evaluating it, so refusals are never cached.
//!
//! State is bounded on every axis - results per player, players tracked, and the
//! player map itself is access-ordered - so a long-running server cannot be made
//! to grow it without bound by reconnecting or by spraying operation ids.

use std::collections::{HashMap, VecDeque};

use tracing::{error, info};

use crate::identity::Id;
use crate::waypoints::model::{self, Draft, LocationKey};
use crate::waypoints::persist::Persistence;
use crate::waypoints::proto::{self, CreateRequest, DeleteRequest, UpdateRequest, Waypoint};
use crate::waypoints::store::{Delta, Prepared, Snapshot, Store, StoreError};

/// Idempotency results retained per player (`IDEMPOTENCY_RESULTS_PER_PLAYER`).
const IDEMPOTENCY_RESULTS_PER_PLAYER: usize = 128;
/// Players whose per-player state is retained at once (`MAX_TRACKED_PLAYERS`).
const MAX_TRACKED_PLAYERS: usize = 256;
/// Mutations a player may issue back to back before the refill rate applies
/// (`MUTATION_BURST`).
const MUTATION_BURST: f64 = 10.0;
/// Attempts to draw an id that is not already in the catalog.
const ID_ATTEMPTS: u32 = 16;

/// Server-side ceilings for the shared-waypoint catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Most points the catalog may hold.
    pub max_per_world: usize,
    /// Most points a single player may have published.
    pub max_per_player: usize,
    /// Sustained mutation rate per player, used to refill the token bucket.
    pub mutations_per_minute: u32,
}

/// Who may create and manage shared waypoints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessPolicy {
    /// Only operators may mutate; a point belongs to no one player.
    OperatorOnly,
    /// A player may publish and manage their own points.
    OwnerManaged,
}

/// The player a mutation is attributed to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Actor {
    /// Their UUID.
    pub id: Id,
    /// Their display name, as of this request.
    pub name: String,
    /// Whether they hold the operator permission.
    pub operator: bool,
}

/// Whether a mutation changed the catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MutationStatus {
    /// The change was written durably and committed.
    Applied,
    /// The change was refused and nothing moved.
    Rejected,
}

/// Why a mutation was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MutationError {
    /// No error.
    None,
    /// The body is not a usable waypoint, or an operation id was reused with a
    /// different body.
    InvalidRequest,
    /// The expected revision did not match the state the request named.
    RevisionConflict,
    /// The named waypoint does not exist.
    NotFound,
    /// The actor may not perform this mutation.
    Forbidden,
    /// The catalog is at its size limit.
    WorldQuotaExceeded,
    /// The actor is at their publish limit.
    PlayerQuotaExceeded,
    /// The actor is mutating faster than their allowance.
    RateLimited,
    /// Another point already occupies the target block.
    DuplicateLocation,
    /// The change could not be written durably, so nothing was committed.
    PersistenceFailed,
    /// No unused id could be drawn.
    IdGenerationFailed,
}

/// The outcome of one mutation.
#[derive(Clone, Debug, PartialEq)]
pub struct MutationResult {
    /// The client's correlation id.
    pub operation_id: Id,
    /// Whether the change was applied.
    pub status: MutationStatus,
    /// Why it was refused, or [`MutationError::None`].
    pub error: MutationError,
    /// The change to broadcast, or a no-op revision for a refusal.
    pub delta: Delta,
    /// Whether this is a retained result replayed for a retried operation.
    pub replayed: bool,
}

impl MutationResult {
    /// Whether the mutation was applied.
    pub fn applied(&self) -> bool {
        self.status == MutationStatus::Applied
    }

    /// The `RESULT_STATUS_*` code for this outcome.
    pub fn status_code(&self) -> i32 {
        match self.status {
            MutationStatus::Applied => proto::RESULT_STATUS_APPLIED,
            MutationStatus::Rejected => proto::RESULT_STATUS_REJECTED,
        }
    }

    /// The `RESULT_ERROR_*` code for this outcome, as the peer can read it.
    ///
    /// `DUPLICATE_LOCATION` was introduced after the first protocol minor; a
    /// peer that predates it is told the request was invalid rather than handed
    /// an error code it has no state for.
    pub fn error_code(&self, negotiated_minor: i32) -> i32 {
        match self.error {
            MutationError::None => proto::RESULT_ERROR_NONE,
            MutationError::InvalidRequest => proto::RESULT_ERROR_INVALID_REQUEST,
            MutationError::RevisionConflict => proto::RESULT_ERROR_REVISION_CONFLICT,
            MutationError::NotFound => proto::RESULT_ERROR_NOT_FOUND,
            MutationError::Forbidden => proto::RESULT_ERROR_FORBIDDEN,
            MutationError::WorldQuotaExceeded => proto::RESULT_ERROR_WORLD_QUOTA_EXCEEDED,
            MutationError::PlayerQuotaExceeded => proto::RESULT_ERROR_PLAYER_QUOTA_EXCEEDED,
            MutationError::RateLimited => proto::RESULT_ERROR_RATE_LIMITED,
            MutationError::PersistenceFailed => proto::RESULT_ERROR_PERSISTENCE_FAILED,
            MutationError::IdGenerationFailed => proto::RESULT_ERROR_ID_GENERATION_FAILED,
            MutationError::DuplicateLocation => {
                if negotiated_minor >= 1 {
                    proto::RESULT_ERROR_DUPLICATE_LOCATION
                } else {
                    proto::RESULT_ERROR_INVALID_REQUEST
                }
            }
        }
    }
}

/// What a mutation does, for the audit line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Create,
    Update,
    Delete,
}

impl Action {
    fn tag(self) -> &'static str {
        match self {
            Action::Create => "CREATE",
            Action::Update => "UPDATE",
            Action::Delete => "DELETE",
        }
    }
}

/// Who is mutating what, as the audit line and the retained result see it.
#[derive(Clone, Copy, Debug)]
struct Mutation<'a> {
    /// The player the mutation is attributed to.
    actor: &'a Actor,
    /// The client's correlation id.
    operation_id: Id,
    /// What the mutation does.
    action: Action,
    /// The point it names, once one has been drawn or found.
    waypoint_id: Option<Id>,
}

/// A request body without its correlation id, which is the cache key instead.
#[derive(Clone, Debug)]
enum RequestBody {
    Create(CreateRequest),
    Update(UpdateRequest),
    Delete(DeleteRequest),
}

impl RequestBody {
    /// Whether two requests carry the same body. The operation id is excluded
    /// because it is the key: reusing one with a changed body is a conflict to
    /// report, not a replay to serve.
    fn matches(&self, other: &RequestBody) -> bool {
        match (self, other) {
            (RequestBody::Create(a), RequestBody::Create(b)) => {
                a.expected_revision == b.expected_revision
                    && a.name == b.name
                    && a.dimension_id == b.dimension_id
                    && a.x == b.x
                    && a.y == b.y
                    && a.z == b.z
                    && a.color == b.color
                    && a.kind == b.kind
                    && a.icon_item_id == b.icon_item_id
                    && a.marker_label == b.marker_label
            }
            (RequestBody::Update(a), RequestBody::Update(b)) => {
                a.id == b.id
                    && a.expected_revision == b.expected_revision
                    && a.name == b.name
                    && a.dimension_id == b.dimension_id
                    && a.x == b.x
                    && a.y == b.y
                    && a.z == b.z
                    && a.color == b.color
                    && a.kind == b.kind
                    && a.icon_item_id == b.icon_item_id
                    && a.marker_label == b.marker_label
            }
            (RequestBody::Delete(a), RequestBody::Delete(b)) => {
                a.id == b.id && a.expected_revision == b.expected_revision
            }
            _ => false,
        }
    }
}

/// One retained applied mutation.
#[derive(Clone, Debug)]
struct CachedMutation {
    body: RequestBody,
    result: MutationResult,
    /// A reused operation id is refused once, then silently, so a stuck client
    /// cannot turn one conflict into a log flood.
    collision_audited: bool,
}

/// Per-player idempotency results and mutation budget.
struct PlayerState {
    results: HashMap<Id, CachedMutation>,
    /// Insertion order of `results`, oldest first, for eviction.
    order: VecDeque<Id>,
    bucket: MutationBucket,
}

impl PlayerState {
    fn new(mutations_per_minute: u32, now_ms: i64) -> Self {
        PlayerState {
            results: HashMap::new(),
            order: VecDeque::new(),
            bucket: MutationBucket::new(MUTATION_BURST, mutations_per_minute, now_ms),
        }
    }

    /// Retains an applied result, dropping the oldest once the cap is reached.
    fn remember(&mut self, operation_id: Id, body: RequestBody, result: MutationResult) {
        if self.order.len() >= IDEMPOTENCY_RESULTS_PER_PLAYER
            && let Some(oldest) = self.order.pop_front()
        {
            self.results.remove(&oldest);
        }
        self.order.push_back(operation_id);
        self.results.insert(
            operation_id,
            CachedMutation {
                body,
                result,
                collision_audited: false,
            },
        );
    }
}

/// A per-player token bucket, in milliseconds.
///
/// Deliberately its own copy rather than the connection-level bucket in
/// `session`: the limit belongs to the player, not to one socket, so
/// reconnecting must not hand out a fresh burst.
#[derive(Clone, Debug)]
struct MutationBucket {
    capacity: f64,
    refill_per_ms: f64,
    tokens: f64,
    last_ms: i64,
}

impl MutationBucket {
    fn new(capacity: f64, refill_per_minute: u32, now_ms: i64) -> Self {
        MutationBucket {
            capacity,
            refill_per_ms: f64::from(refill_per_minute) / 60_000.0,
            tokens: capacity,
            last_ms: now_ms,
        }
    }

    fn try_consume(&mut self, now_ms: i64) -> bool {
        if now_ms > self.last_ms {
            let elapsed = (now_ms - self.last_ms) as f64;
            self.tokens = self
                .capacity
                .min(self.tokens + elapsed * self.refill_per_ms);
            self.last_ms = now_ms;
        }
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}

/// Every player with retained state, ordered by last use.
#[derive(Default)]
struct TrackedPlayers {
    order: VecDeque<Id>,
    states: HashMap<Id, PlayerState>,
}

impl TrackedPlayers {
    /// Returns the player's state, creating it if needed and marking the player
    /// as most recently used. The cap evicts the coldest peer.
    fn touch(&mut self, key: Id, now_ms: i64, mutations_per_minute: u32) -> &mut PlayerState {
        if let Some(position) = self.order.iter().position(|candidate| *candidate == key) {
            self.order.remove(position);
        } else if self.states.len() >= MAX_TRACKED_PLAYERS
            && let Some(evicted) = self.order.pop_front()
        {
            self.states.remove(&evicted);
        }
        self.order.push_back(key);
        self.states
            .entry(key)
            .or_insert_with(|| PlayerState::new(mutations_per_minute, now_ms))
    }

    fn state_mut(&mut self, key: Id) -> Option<&mut PlayerState> {
        self.states.get_mut(&key)
    }

    fn len(&self) -> usize {
        self.states.len()
    }
}

/// The authoritative mutation service for one world's shared waypoints.
pub struct Service {
    store: Store,
    persistence: Persistence,
    limits: Limits,
    access: AccessPolicy,
    players: TrackedPlayers,
}

impl Service {
    /// Builds a service over a loaded catalog.
    ///
    /// A snapshot that no longer validates against the active world should be
    /// run through [`sanitize_loaded`] first; the service itself trusts the
    /// store it is handed.
    pub fn new(
        store: Store,
        persistence: Persistence,
        limits: Limits,
        access: AccessPolicy,
    ) -> Self {
        Service {
            store,
            persistence,
            limits,
            access,
            players: TrackedPlayers::default(),
        }
    }

    /// A copy of the whole catalog.
    pub fn snapshot(&self) -> Snapshot {
        self.store.snapshot()
    }

    /// The configured access policy.
    pub fn access_policy(&self) -> AccessPolicy {
        self.access
    }

    /// How many players have retained per-player state.
    ///
    /// For diagnostics and tests: it exposes the bound on retained idempotency
    /// state without exposing the state itself.
    pub fn tracked_player_count(&self) -> usize {
        self.players.len()
    }

    /// Applies a create request.
    ///
    /// The check order is the reference order: idempotency, permission, rate
    /// limit, revision, validation, occupancy, both quotas, id, then the write.
    pub fn create(&mut self, actor: &Actor, request: &CreateRequest) -> MutationResult {
        let Service {
            store,
            persistence,
            limits,
            access,
            players,
        } = self;
        let access = *access;
        let operation_id = request.operation_id;
        let now = crate::clock::now_ms();
        let key = actor.id;
        let body = RequestBody::Create(request.clone());
        let mutation = Mutation {
            actor,
            operation_id,
            action: Action::Create,
            waypoint_id: None,
        };

        {
            let player = players.touch(key, now, limits.mutations_per_minute);
            if let Some(replay) = replay_decision(player, &body, store.revision(), mutation) {
                return replay;
            }
            if !can_create(actor, access) {
                return finish(
                    Some(player),
                    body,
                    mutation,
                    reject(operation_id, store.revision(), MutationError::Forbidden),
                    now,
                );
            }
            if !player.bucket.try_consume(now) {
                return finish(
                    Some(player),
                    body,
                    mutation,
                    reject(operation_id, store.revision(), MutationError::RateLimited),
                    now,
                );
            }
        }

        if request.expected_revision != store.revision() {
            return finish(
                players.state_mut(key),
                body,
                mutation,
                reject(
                    operation_id,
                    store.revision(),
                    MutationError::RevisionConflict,
                ),
                now,
            );
        }
        let draft = Draft {
            name: request.name.clone(),
            dimension_id: request.dimension_id.clone(),
            x: request.x,
            y: request.y,
            z: request.z,
            color: request.color,
            kind: request.kind,
            icon_item_id: request.icon_item_id.clone(),
            marker_label: request.marker_label.clone(),
        };
        let Some(validated) = model::validate(&draft) else {
            return finish(
                players.state_mut(key),
                body,
                mutation,
                reject(
                    operation_id,
                    store.revision(),
                    MutationError::InvalidRequest,
                ),
                now,
            );
        };
        let publisher_name = actor.name.trim();
        if !model::valid_publisher_name(publisher_name) {
            return finish(
                players.state_mut(key),
                body,
                mutation,
                reject(
                    operation_id,
                    store.revision(),
                    MutationError::InvalidRequest,
                ),
                now,
            );
        }
        let Some(location) = LocationKey::of(
            &validated.dimension_id,
            validated.x,
            validated.y,
            validated.z,
        ) else {
            // Unreachable after validation; the finite check is immediate.
            return finish(
                players.state_mut(key),
                body,
                mutation,
                reject(
                    operation_id,
                    store.revision(),
                    MutationError::InvalidRequest,
                ),
                now,
            );
        };
        if store.find_at(&location).is_some() {
            return finish(
                players.state_mut(key),
                body,
                mutation,
                reject(
                    operation_id,
                    store.revision(),
                    MutationError::DuplicateLocation,
                ),
                now,
            );
        }
        if store.len() >= limits.max_per_world {
            return finish(
                players.state_mut(key),
                body,
                mutation,
                reject(
                    operation_id,
                    store.revision(),
                    MutationError::WorldQuotaExceeded,
                ),
                now,
            );
        }
        if store.count_published_by(actor.id) >= limits.max_per_player {
            return finish(
                players.state_mut(key),
                body,
                mutation,
                reject(
                    operation_id,
                    store.revision(),
                    MutationError::PlayerQuotaExceeded,
                ),
                now,
            );
        }
        let Some(id) = unique_id(store) else {
            return finish(
                players.state_mut(key),
                body,
                mutation,
                reject(
                    operation_id,
                    store.revision(),
                    MutationError::IdGenerationFailed,
                ),
                now,
            );
        };
        let Some(next_revision) = store.revision().checked_add(1) else {
            return finish(
                players.state_mut(key),
                body,
                Mutation {
                    waypoint_id: Some(id),
                    ..mutation
                },
                reject(
                    operation_id,
                    store.revision(),
                    MutationError::PersistenceFailed,
                ),
                now,
            );
        };
        let waypoint = Waypoint {
            id,
            publisher_id: actor.id,
            publisher_name: publisher_name.to_string(),
            name: validated.name,
            dimension_id: validated.dimension_id,
            x: validated.x,
            y: validated.y,
            z: validated.z,
            color_argb: validated.color,
            kind: validated.kind,
            icon_item_id: validated.icon_item_id,
            marker_label: validated.marker_label,
            created_at_ms: now,
            revision: next_revision,
        };
        let prepared = match store.prepare_create(waypoint) {
            Ok(prepared) => prepared,
            Err(error) => {
                return finish(
                    players.state_mut(key),
                    body,
                    Mutation {
                        waypoint_id: Some(id),
                        ..mutation
                    },
                    reject(operation_id, store.revision(), store_error(error)),
                    now,
                );
            }
        };
        let result = match persist(store, persistence, prepared, operation_id, actor) {
            Some(delta) => applied(operation_id, delta),
            None => reject(
                operation_id,
                store.revision(),
                MutationError::PersistenceFailed,
            ),
        };
        finish(
            players.state_mut(key),
            body,
            Mutation {
                waypoint_id: Some(id),
                ..mutation
            },
            result,
            now,
        )
    }

    /// Applies a delete request.
    ///
    /// The reference order here is idempotency, rate limit, existence,
    /// revision, then permission: a caller learns a point is gone before it
    /// learns it may not touch it.
    pub fn delete(&mut self, actor: &Actor, request: &DeleteRequest) -> MutationResult {
        let Service {
            store,
            persistence,
            limits,
            access,
            players,
        } = self;
        let access = *access;
        let operation_id = request.operation_id;
        let now = crate::clock::now_ms();
        let key = actor.id;
        let body = RequestBody::Delete(request.clone());
        let mutation = Mutation {
            actor,
            operation_id,
            action: Action::Delete,
            waypoint_id: Some(request.id),
        };

        {
            let player = players.touch(key, now, limits.mutations_per_minute);
            if let Some(replay) = replay_decision(player, &body, store.revision(), mutation) {
                return replay;
            }
            if !player.bucket.try_consume(now) {
                return finish(
                    Some(player),
                    body,
                    mutation,
                    reject(operation_id, store.revision(), MutationError::RateLimited),
                    now,
                );
            }
        }

        let Some(current) = store.find(request.id).cloned() else {
            return finish(
                players.state_mut(key),
                body,
                mutation,
                reject(operation_id, store.revision(), MutationError::NotFound),
                now,
            );
        };
        if request.expected_revision != current.revision {
            return finish(
                players.state_mut(key),
                body,
                mutation,
                reject(
                    operation_id,
                    store.revision(),
                    MutationError::RevisionConflict,
                ),
                now,
            );
        }
        if !can_manage(actor, &current, access) {
            return finish(
                players.state_mut(key),
                body,
                mutation,
                reject(operation_id, store.revision(), MutationError::Forbidden),
                now,
            );
        }
        let prepared = match store.prepare_delete(request.id) {
            Ok(prepared) => prepared,
            Err(error) => {
                return finish(
                    players.state_mut(key),
                    body,
                    mutation,
                    reject(operation_id, store.revision(), store_error(error)),
                    now,
                );
            }
        };
        let result = match persist(store, persistence, prepared, operation_id, actor) {
            Some(delta) => applied(operation_id, delta),
            None => reject(
                operation_id,
                store.revision(),
                MutationError::PersistenceFailed,
            ),
        };
        finish(players.state_mut(key), body, mutation, result, now)
    }

    /// Applies an update request.
    ///
    /// The reference order here is idempotency, rate limit, existence,
    /// permission, then revision - an update names a point it does not own more
    /// often than one that moved underneath it, so the ownership answer is the
    /// more useful one. The publisher name is preserved rather than re-checked
    /// because the update does not re-publish the point.
    pub fn update(&mut self, actor: &Actor, request: &UpdateRequest) -> MutationResult {
        let Service {
            store,
            persistence,
            limits,
            access,
            players,
        } = self;
        let access = *access;
        let operation_id = request.operation_id;
        let now = crate::clock::now_ms();
        let key = actor.id;
        let body = RequestBody::Update(request.clone());
        let mutation = Mutation {
            actor,
            operation_id,
            action: Action::Update,
            waypoint_id: Some(request.id),
        };

        {
            let player = players.touch(key, now, limits.mutations_per_minute);
            if let Some(replay) = replay_decision(player, &body, store.revision(), mutation) {
                return replay;
            }
            if !player.bucket.try_consume(now) {
                return finish(
                    Some(player),
                    body,
                    mutation,
                    reject(operation_id, store.revision(), MutationError::RateLimited),
                    now,
                );
            }
        }

        let Some(current) = store.find(request.id).cloned() else {
            return finish(
                players.state_mut(key),
                body,
                mutation,
                reject(operation_id, store.revision(), MutationError::NotFound),
                now,
            );
        };
        if !can_manage(actor, &current, access) {
            return finish(
                players.state_mut(key),
                body,
                mutation,
                reject(operation_id, store.revision(), MutationError::Forbidden),
                now,
            );
        }
        if request.expected_revision != current.revision {
            return finish(
                players.state_mut(key),
                body,
                mutation,
                reject(
                    operation_id,
                    store.revision(),
                    MutationError::RevisionConflict,
                ),
                now,
            );
        }
        let draft = Draft {
            name: request.name.clone(),
            dimension_id: request.dimension_id.clone(),
            x: request.x,
            y: request.y,
            z: request.z,
            color: request.color,
            kind: request.kind,
            icon_item_id: request.icon_item_id.clone(),
            marker_label: request.marker_label.clone(),
        };
        let Some(validated) = model::validate(&draft) else {
            return finish(
                players.state_mut(key),
                body,
                mutation,
                reject(
                    operation_id,
                    store.revision(),
                    MutationError::InvalidRequest,
                ),
                now,
            );
        };
        let Some(updated_location) = LocationKey::of(
            &validated.dimension_id,
            validated.x,
            validated.y,
            validated.z,
        ) else {
            return finish(
                players.state_mut(key),
                body,
                mutation,
                reject(
                    operation_id,
                    store.revision(),
                    MutationError::InvalidRequest,
                ),
                now,
            );
        };
        // Checked explicitly, not only inside `prepare_update`, so the error a
        // client sees matches the reference for a move onto an occupied block.
        let current_location = LocationKey::from(&current);
        if updated_location != current_location
            && store
                .find_at(&updated_location)
                .is_some_and(|occupant| occupant.id != current.id)
        {
            return finish(
                players.state_mut(key),
                body,
                mutation,
                reject(
                    operation_id,
                    store.revision(),
                    MutationError::DuplicateLocation,
                ),
                now,
            );
        }
        let Some(next_revision) = store.revision().checked_add(1) else {
            return finish(
                players.state_mut(key),
                body,
                mutation,
                reject(
                    operation_id,
                    store.revision(),
                    MutationError::PersistenceFailed,
                ),
                now,
            );
        };
        let updated = Waypoint {
            id: current.id,
            publisher_id: current.publisher_id,
            publisher_name: current.publisher_name.clone(),
            name: validated.name,
            dimension_id: validated.dimension_id,
            x: validated.x,
            y: validated.y,
            z: validated.z,
            color_argb: validated.color,
            kind: validated.kind,
            icon_item_id: validated.icon_item_id,
            marker_label: validated.marker_label,
            created_at_ms: current.created_at_ms,
            revision: next_revision,
        };
        let prepared = match store.prepare_update(updated) {
            Ok(prepared) => prepared,
            Err(error) => {
                return finish(
                    players.state_mut(key),
                    body,
                    mutation,
                    reject(operation_id, store.revision(), store_error(error)),
                    now,
                );
            }
        };
        let result = match persist(store, persistence, prepared, operation_id, actor) {
            Some(delta) => applied(operation_id, delta),
            None => reject(
                operation_id,
                store.revision(),
                MutationError::PersistenceFailed,
            ),
        };
        finish(players.state_mut(key), body, mutation, result, now)
    }
}

/// Drops persisted waypoints that no longer validate against the active world.
///
/// A removed datapack dimension, or a tightened display rule, can strand a
/// stored point that this build would refuse to create. One such entry must not
/// disable the whole feature, so it is quarantined here and reported: the
/// server stays authoritative, and the surviving entries keep the loaded
/// revision because nothing observable about them changed. The caller is
/// expected to log the warnings.
pub fn sanitize_loaded(snapshot: Snapshot) -> (Snapshot, Vec<String>) {
    let revision = snapshot.revision;
    let total = snapshot.waypoints.len();
    let mut kept = Vec::with_capacity(total);
    let mut warnings = Vec::new();
    for waypoint in snapshot.waypoints {
        if valid_for_active_world(&waypoint) {
            kept.push(waypoint);
        } else {
            warnings.push(format!(
                "quarantining persisted shared waypoint {} in {}: invalid for the active world",
                waypoint.id, waypoint.dimension_id
            ));
        }
    }
    (
        Snapshot {
            revision,
            waypoints: kept,
        },
        warnings,
    )
}

/// Whether a stored point would still pass validation for the active world.
fn valid_for_active_world(waypoint: &Waypoint) -> bool {
    let draft = Draft {
        name: waypoint.name.clone(),
        dimension_id: waypoint.dimension_id.clone(),
        x: waypoint.x,
        y: waypoint.y,
        z: waypoint.z,
        color: waypoint.color_argb,
        kind: waypoint.kind,
        icon_item_id: waypoint.icon_item_id.clone(),
        marker_label: waypoint.marker_label.clone(),
    };
    model::validate(&draft).is_some() && model::valid_publisher_name(&waypoint.publisher_name)
}

/// Resolves a retried operation, or reports a reused id as a conflict.
///
/// Returns `None` for an operation this player has not applied. A matching body
/// replays the retained result; a changed body is a collision, audited once.
fn replay_decision(
    player: &mut PlayerState,
    body: &RequestBody,
    revision: i64,
    mutation: Mutation<'_>,
) -> Option<MutationResult> {
    match player.results.get(&mutation.operation_id) {
        None => return None,
        Some(cached) if cached.body.matches(body) => {
            let mut result = cached.result.clone();
            result.replayed = true;
            return Some(result);
        }
        Some(_) => {}
    }
    let first_collision = match player.results.get_mut(&mutation.operation_id) {
        Some(cached) => {
            if cached.collision_audited {
                false
            } else {
                cached.collision_audited = true;
                true
            }
        }
        None => false,
    };
    let collision = reject(
        mutation.operation_id,
        revision,
        MutationError::InvalidRequest,
    );
    if first_collision {
        audit(
            &mutation,
            collision.status,
            collision.error,
            revision,
            crate::clock::now_ms(),
        );
    }
    Some(collision)
}

/// Retains an applied result and audits every outcome.
fn finish(
    player: Option<&mut PlayerState>,
    body: RequestBody,
    mutation: Mutation<'_>,
    result: MutationResult,
    now: i64,
) -> MutationResult {
    // Only applied results are retained. Caching a refusal would replay a
    // transient `RATE_LIMITED` or `PERSISTENCE_FAILED` forever, and would keep a
    // full result per refusal.
    if result.applied()
        && let Some(player) = player
    {
        player.remember(mutation.operation_id, body, result.clone());
    }
    audit(
        &mutation,
        result.status,
        result.error,
        result.delta.revision,
        now,
    );
    result
}

/// Writes the prepared state, then commits it; a failed write commits nothing.
fn persist(
    store: &mut Store,
    persistence: &Persistence,
    prepared: Prepared,
    operation_id: Id,
    actor: &Actor,
) -> Option<Delta> {
    if let Err(reason) = persistence.save(prepared.snapshot()) {
        error!(
            operation_id = %operation_id,
            actor_id = %actor.id,
            "shared waypoint persistence failed, nothing committed: {}",
            reason
        );
        return None;
    }
    let delta = prepared.delta().clone();
    if let Err(error) = store.commit(prepared) {
        // Unreachable while the service holds its own store: the mutation was
        // prepared against the current revision and nothing can interleave.
        error!(
            operation_id = %operation_id,
            actor_id = %actor.id,
            "shared waypoint commit failed after a successful write: {:?}",
            error
        );
        return None;
    }
    Some(delta)
}

/// A committed outcome.
fn applied(operation_id: Id, delta: Delta) -> MutationResult {
    MutationResult {
        operation_id,
        status: MutationStatus::Applied,
        error: MutationError::None,
        delta,
        replayed: false,
    }
}

/// A refused outcome that moved nothing.
fn reject(operation_id: Id, revision: i64, error: MutationError) -> MutationResult {
    MutationResult {
        operation_id,
        status: MutationStatus::Rejected,
        error,
        delta: Delta::noop(revision),
        replayed: false,
    }
}

/// Emits the audit line for one mutation.
///
/// Names and coordinates are deliberately absent: this log may be read by
/// someone other than the players, and the reference audit event excludes
/// player content for the same reason.
fn audit(
    mutation: &Mutation<'_>,
    status: MutationStatus,
    error: MutationError,
    revision: i64,
    now: i64,
) {
    let waypoint = mutation
        .waypoint_id
        .map(|id| id.to_string())
        .unwrap_or_else(|| "none".to_string());
    info!(
        operation_id = %mutation.operation_id,
        actor_id = %mutation.actor.id,
        action = mutation.action.tag(),
        status = ?status,
        error = ?error,
        waypoint_id = %waypoint,
        revision = revision,
        at = now,
        "shared waypoint mutation"
    );
}

/// Whether the actor may publish at all.
fn can_create(actor: &Actor, access: AccessPolicy) -> bool {
    actor.operator || access == AccessPolicy::OwnerManaged
}

/// Whether the actor may change a specific point.
fn can_manage(actor: &Actor, waypoint: &Waypoint, access: AccessPolicy) -> bool {
    actor.operator || (access == AccessPolicy::OwnerManaged && waypoint.publisher_id == actor.id)
}

/// Draws an id the catalog does not already hold.
fn unique_id(store: &Store) -> Option<Id> {
    for _ in 0..ID_ATTEMPTS {
        let candidate = Id::random();
        if store.find(candidate).is_none() {
            return Some(candidate);
        }
    }
    None
}

/// Maps a store refusal to the error a client is told.
fn store_error(error: StoreError) -> MutationError {
    match error {
        StoreError::DuplicateLocation => MutationError::DuplicateLocation,
        StoreError::Missing => MutationError::NotFound,
        // The id was drawn again only after colliding with the catalog, which is
        // the same condition `unique_id` reports.
        StoreError::DuplicateId => MutationError::IdGenerationFailed,
        StoreError::RevisionExhausted | StoreError::Stale | StoreError::Malformed => {
            MutationError::PersistenceFailed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::waypoints::proto::WaypointKind;
    use crate::waypoints::store::DeltaKind;

    const OVERWORLD: &str = "minecraft:overworld";

    fn actor(id: u64, operator: bool) -> Actor {
        Actor {
            id: Id { high: 0, low: id },
            name: format!("player{id}"),
            operator,
        }
    }

    fn limits(max_per_world: usize, max_per_player: usize, per_minute: u32) -> Limits {
        Limits {
            max_per_world,
            max_per_player,
            mutations_per_minute: per_minute,
        }
    }

    fn scratch(name: &str) -> String {
        let folder = std::env::temp_dir().join(format!(
            "cfm-service-{name}-{}-{}",
            std::process::id(),
            Id::random()
        ));
        std::fs::create_dir_all(&folder).expect("temp dir");
        folder.to_string_lossy().to_string()
    }

    fn service(folder: &str, limits: Limits, access: AccessPolicy) -> Service {
        Service::new(
            Store::empty(),
            Persistence::new(folder, None),
            limits,
            access,
        )
    }

    fn create_request(operation: u64, expected_revision: i64, x: f64) -> CreateRequest {
        CreateRequest {
            operation_id: Id {
                high: 0,
                low: operation,
            },
            expected_revision,
            name: "Base".to_string(),
            dimension_id: OVERWORLD.to_string(),
            x,
            y: 64.0,
            z: 0.0,
            color: 0xff34_98dbu32 as i32,
            kind: WaypointKind::Normal,
            icon_item_id: String::new(),
            marker_label: String::new(),
        }
    }

    fn waypoint(id: u64, revision: i64) -> Waypoint {
        Waypoint {
            id: Id { high: 0, low: id },
            publisher_id: Id {
                high: 0,
                low: 0xaaaa,
            },
            publisher_name: "Steve".to_string(),
            name: format!("point {id}"),
            dimension_id: OVERWORLD.to_string(),
            x: 12.5,
            y: 64.0,
            z: -7.25,
            color_argb: 0xff34_98dbu32 as i32,
            kind: WaypointKind::Normal,
            icon_item_id: "minecraft:compass".to_string(),
            marker_label: "B".to_string(),
            created_at_ms: 1_712_345_678_901,
            revision,
        }
    }

    #[test]
    fn a_create_is_committed_and_advances_the_revision() {
        let folder = scratch("create");
        let mut service = service(&folder, limits(512, 64, 600), AccessPolicy::OwnerManaged);
        let author = actor(1, false);

        let result = service.create(&author, &create_request(1, 0, 12.5));
        assert!(result.applied(), "{:?}", result.error);
        assert_eq!(result.error, MutationError::None);
        assert!(!result.replayed);
        assert_eq!(result.delta.kind, DeltaKind::Upsert);
        assert_eq!(service.snapshot().revision, 1);
        assert_eq!(service.snapshot().waypoints.len(), 1);

        let created = result.delta.waypoint.expect("an upsert carries the point");
        assert_eq!(created.revision, 1);
        assert_eq!(created.publisher_id, author.id);
        assert_eq!(created.publisher_name, "player1");
        assert!(created.created_at_ms > 0, "the service stamps the time");
        fs_cleanup(&folder);
    }

    #[test]
    fn a_stale_expected_revision_is_refused() {
        let folder = scratch("revision");
        let mut service = service(&folder, limits(512, 64, 600), AccessPolicy::OwnerManaged);
        let author = actor(1, true);

        assert!(
            service
                .create(&author, &create_request(1, 0, 12.5))
                .applied()
        );
        let stale = service.create(&author, &create_request(2, 0, 40.0));
        assert_eq!(stale.error, MutationError::RevisionConflict);
        assert_eq!(service.snapshot().revision, 1, "a refusal moves nothing");
        assert_eq!(service.snapshot().waypoints.len(), 1);
        fs_cleanup(&folder);
    }

    #[test]
    fn a_second_point_in_the_same_block_is_refused() {
        let folder = scratch("duplicate");
        let mut service = service(&folder, limits(512, 64, 600), AccessPolicy::OwnerManaged);
        let author = actor(1, true);

        assert!(
            service
                .create(&author, &create_request(1, 0, 12.5))
                .applied()
        );
        // 12.9 floors into the block 12.5 already occupies.
        let duplicate = service.create(&author, &create_request(2, 1, 12.9));
        assert_eq!(duplicate.error, MutationError::DuplicateLocation);
        assert_eq!(service.snapshot().revision, 1);
        fs_cleanup(&folder);
    }

    #[test]
    fn the_world_and_player_quotas_are_both_enforced() {
        let folder = scratch("world-quota");
        let mut world = service(&folder, limits(1, 1, 600), AccessPolicy::OwnerManaged);
        let author = actor(1, true);
        assert!(world.create(&author, &create_request(1, 0, 12.5)).applied());
        assert_eq!(
            world.create(&author, &create_request(2, 1, 40.0)).error,
            MutationError::WorldQuotaExceeded
        );
        fs_cleanup(&folder);

        let folder = scratch("player-quota");
        let mut player = service(&folder, limits(10, 1, 600), AccessPolicy::OwnerManaged);
        assert!(
            player
                .create(&author, &create_request(1, 0, 12.5))
                .applied()
        );
        assert_eq!(
            player.create(&author, &create_request(2, 1, 40.0)).error,
            MutationError::PlayerQuotaExceeded
        );
        fs_cleanup(&folder);
    }

    #[test]
    fn operator_only_refuses_a_non_operator() {
        let folder = scratch("forbidden");
        let mut service = service(&folder, limits(512, 64, 600), AccessPolicy::OperatorOnly);

        let refused = service.create(&actor(1, false), &create_request(1, 0, 12.5));
        assert!(!refused.applied());
        assert_eq!(refused.error, MutationError::Forbidden);
        assert_eq!(service.snapshot().revision, 0);

        // The same request from an operator is accepted.
        assert!(
            service
                .create(&actor(2, true), &create_request(2, 0, 12.5))
                .applied()
        );
        fs_cleanup(&folder);
    }

    #[test]
    fn a_retried_operation_replays_without_touching_the_catalog() {
        let folder = scratch("replay");
        let mut service = service(&folder, limits(512, 64, 600), AccessPolicy::OwnerManaged);
        let author = actor(1, true);
        let request = create_request(7, 0, 12.5);

        let first = service.create(&author, &request);
        assert!(first.applied());
        assert!(!first.replayed);
        let revision = service.snapshot().revision;

        let replay = service.create(&author, &request);
        assert!(replay.applied());
        assert!(
            replay.replayed,
            "a retried operation must be reported as a replay"
        );
        assert_eq!(service.snapshot().revision, revision);
        assert_eq!(service.snapshot().waypoints.len(), 1);

        // The same operation id with a changed body is a conflict, not a replay.
        let mut changed = request.clone();
        changed.name = "Renamed".to_string();
        let conflict = service.create(&author, &changed);
        assert!(!conflict.applied());
        assert_eq!(conflict.error, MutationError::InvalidRequest);
        assert_eq!(service.snapshot().revision, revision);
        assert_eq!(service.snapshot().waypoints.len(), 1);
        fs_cleanup(&folder);
    }

    #[test]
    fn a_burst_larger_than_the_bucket_is_rate_limited() {
        let folder = scratch("rate");
        // One mutation a minute means the bucket cannot refill within a test, so
        // the eleventh call is refused deterministically.
        let mut service = service(&folder, limits(512, 64, 1), AccessPolicy::OwnerManaged);
        let author = actor(1, true);
        for index in 0..10u64 {
            let result = service.create(
                &author,
                &create_request(index + 1, index as i64, index as f64 * 16.0),
            );
            assert!(result.applied(), "{:?}", result.error);
        }
        let limited = service.create(&author, &create_request(99, 10, 1000.0));
        assert_eq!(limited.error, MutationError::RateLimited);
        assert_eq!(
            service.snapshot().waypoints.len(),
            10,
            "nothing extra landed"
        );
        fs_cleanup(&folder);
    }

    #[test]
    fn a_failed_write_commits_nothing() {
        // The parent directory does not exist, so the write cannot succeed.
        let missing = std::env::temp_dir()
            .join(format!("cfm-service-missing-{}", Id::random()))
            .to_string_lossy()
            .to_string();
        let mut service = Service::new(
            Store::empty(),
            Persistence::new(&missing, None),
            limits(512, 64, 600),
            AccessPolicy::OwnerManaged,
        );

        let result = service.create(&actor(1, true), &create_request(1, 0, 12.5));
        assert!(!result.applied());
        assert_eq!(result.error, MutationError::PersistenceFailed);
        assert_eq!(service.snapshot().revision, 0, "the store must not move");
        assert_eq!(service.snapshot().waypoints.len(), 0);
    }

    #[test]
    fn a_non_owner_cannot_delete_someone_elses_point() {
        let folder = scratch("delete-forbidden");
        let mut service = service(&folder, limits(512, 64, 600), AccessPolicy::OwnerManaged);
        let owner = actor(1, false);
        let created = service.create(&owner, &create_request(1, 0, 12.5));
        let id = created.delta.waypoint.expect("the point").id;

        let request = DeleteRequest {
            operation_id: Id { high: 0, low: 2 },
            id,
            expected_revision: 1,
        };
        let refused = service.delete(&actor(9, false), &request);
        assert_eq!(refused.error, MutationError::Forbidden);
        assert_eq!(service.snapshot().revision, 1);

        // The owner may remove it, and the revision advances.
        let owned = DeleteRequest {
            operation_id: Id { high: 0, low: 3 },
            ..request
        };
        let removed = service.delete(&owner, &owned);
        assert!(removed.applied(), "{:?}", removed.error);
        assert_eq!(removed.delta.kind, DeltaKind::Remove);
        assert_eq!(service.snapshot().revision, 2);
        assert_eq!(service.snapshot().waypoints.len(), 0);
        fs_cleanup(&folder);
    }

    #[test]
    fn an_update_preserves_the_original_publisher() {
        let folder = scratch("update");
        let mut service = service(&folder, limits(512, 64, 600), AccessPolicy::OwnerManaged);
        let owner = actor(1, false);
        let created = service.create(&owner, &create_request(1, 0, 12.5));
        let original = created.delta.waypoint.expect("the point");

        let request = UpdateRequest {
            operation_id: Id { high: 0, low: 2 },
            id: original.id,
            expected_revision: 1,
            name: "Renamed".to_string(),
            dimension_id: OVERWORLD.to_string(),
            x: 12.5,
            y: 64.0,
            z: 0.0,
            color: 0xff00_ff00u32 as i32,
            kind: WaypointKind::Normal,
            icon_item_id: String::new(),
            marker_label: String::new(),
        };
        let updated = service.update(&owner, &request);
        assert!(updated.applied(), "{:?}", updated.error);
        let waypoint = updated.delta.waypoint.expect("the point");
        assert_eq!(
            waypoint.id, original.id,
            "the id is server-owned and stable"
        );
        assert_eq!(waypoint.publisher_id, owner.id);
        assert_eq!(waypoint.publisher_name, original.publisher_name);
        assert_eq!(waypoint.created_at_ms, original.created_at_ms);
        assert_eq!(waypoint.revision, 2);
        assert_eq!(service.snapshot().revision, 2);
        fs_cleanup(&folder);
    }

    #[test]
    fn duplicate_location_is_downgraded_for_a_legacy_peer() {
        let result = reject(Id { high: 0, low: 1 }, 0, MutationError::DuplicateLocation);
        assert_eq!(result.status_code(), proto::RESULT_STATUS_REJECTED);
        assert_eq!(result.error_code(0), proto::RESULT_ERROR_INVALID_REQUEST);
        assert_eq!(result.error_code(1), proto::RESULT_ERROR_DUPLICATE_LOCATION);
        assert_eq!(result.error_code(3), proto::RESULT_ERROR_DUPLICATE_LOCATION);
    }

    #[test]
    fn sanitize_drops_points_that_no_longer_validate() {
        let good = waypoint(1, 1);
        let mut bad = good.clone();
        bad.id = Id { high: 0, low: 2 };
        bad.dimension_id = "Not A Dimension".to_string();
        bad.revision = 2;
        let snapshot = Snapshot {
            revision: 2,
            waypoints: vec![good.clone(), bad],
        };

        let (sanitized, warnings) = sanitize_loaded(snapshot);
        assert_eq!(sanitized.revision, 2, "survivors keep the loaded revision");
        assert_eq!(sanitized.waypoints.len(), 1);
        assert_eq!(sanitized.waypoints[0].id, good.id);
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("invalid for the active world"),
            "{warnings:?}"
        );
    }

    #[test]
    fn a_point_with_an_unusable_publisher_name_is_sanitized_away() {
        let mut bad = waypoint(1, 1);
        bad.publisher_name = "bad\nname".to_string();
        let (sanitized, warnings) = sanitize_loaded(Snapshot {
            revision: 1,
            waypoints: vec![bad],
        });
        assert!(sanitized.waypoints.is_empty());
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn the_player_cap_evicts_the_least_recently_used() {
        let mut players = TrackedPlayers::default();
        for index in 0..MAX_TRACKED_PLAYERS as u64 {
            players.touch(
                Id {
                    high: 0,
                    low: index,
                },
                0,
                600,
            );
        }
        assert_eq!(players.len(), MAX_TRACKED_PLAYERS);

        // Refreshing a key makes a colder one the eviction victim.
        players.touch(Id { high: 0, low: 0 }, 0, 600);
        players.touch(Id { high: 0, low: 999 }, 0, 600);
        assert_eq!(players.len(), MAX_TRACKED_PLAYERS);
        assert!(
            players.state_mut(Id { high: 0, low: 0 }).is_some(),
            "recent use survives"
        );
        assert!(
            players.state_mut(Id { high: 0, low: 1 }).is_none(),
            "the coldest key is gone"
        );
        assert!(players.state_mut(Id { high: 0, low: 999 }).is_some());
    }

    fn fs_cleanup(folder: &str) {
        let _ = std::fs::remove_dir_all(folder);
    }
}
