//! The shared-waypoint feature: one catalog, many connections.
//!
//! This module is the feature's hub - the only place that holds a lock across
//! the catalog and the live sessions, and the only place that talks to the host.
//! Everything else is a leaf: [`proto`] moves bytes, [`model`] decides what a
//! point may be, [`store`] numbers revisions, [`persist`] writes the file,
//! [`service`] owns permissions and quotas, [`session`] remembers what each
//! connection negotiated.
//!
//! # The one rule about locking
//!
//! **Take the lock, compute, release it, then call the host.** The host
//! re-enters the plugin from the network thread, so `send_custom_payload` or
//! `get_all_players` must never run while a lock is held - and a session's
//! negotiated minor has to be read out under the lock and carried out of it,
//! because it is what every outgoing frame is encoded against.
//!
//! # Why a broadcast needs the server
//!
//! A mutation sends one `RESULT` back to its author and one delta to *every*
//! subscribed peer - the author included, since it is subscribed too. That means
//! the payload path needs the server handle to enumerate players, which is why
//! [`on_payload`] takes one.

pub mod model;
pub mod persist;
pub mod proto;
pub mod service;
pub mod session;
pub mod store;

pub use proto::CHANNEL_ID;

use std::fmt::Write as _;
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::identity::Id;
use pumpkin_plugin_api::{Player, Server};
use tracing::{debug, info, warn};

use crate::clock::now_ms;
use crate::config::Config;

use persist::Persistence;
use proto::{Inbound, Outbound, Waypoint};
use service::{AccessPolicy, Actor, Limits, MutationResult, Service};
use session::{Peer, Sessions};
use store::{Delta, DeltaKind, Snapshot, Store};

/// Longest snapshot this build will send, matching `MAX_S2C_PAYLOAD`.
const MAX_S2C_PAYLOAD: usize = 1 << 20;
/// How many points `/cfm waypoints list` shows per page.
const PAGE_SIZE: usize = 6;

/// What the operator asked for, resolved from the configuration.
#[derive(Clone, Debug)]
struct Settings {
    /// `share_waypoints`.
    enabled: bool,
    /// Cache namespace reported to clients, the same one the policy uses.
    world_id: String,
    /// Quotas and pacing.
    limits: Limits,
    /// Who may manage what.
    access: AccessPolicy,
}

/// Counters for `/cfm waypoints`.
#[derive(Clone, Copy, Debug, Default)]
struct Counters {
    hellos: u64,
    subscribes: u64,
    applied: u64,
    rejected: u64,
    malformed: u64,
    muted: u64,
    broadcasts: u64,
    encode_failures: u64,
}

/// The catalog, the sessions, and the settings that tie them together.
struct Hub {
    sessions: Sessions,
    /// `None` only before the first [`configure`].
    service: Option<Service>,
    settings: Settings,
    counters: Counters,
    /// Where the catalog file lives, so the hub can rebuild its persistence.
    data_folder: String,
    /// The instance id stamped into the catalog file.
    owner_instance_id: Option<String>,
    /// Set when the settings changed, so the next tick tells every compatible
    /// peer about it without needing a server handle here.
    refresh_status: bool,
    /// Deltas an administrative clear produced, sent on the next tick in order.
    pending_deltas: Vec<Outbound>,
}

static HUB: OnceLock<Mutex<Hub>> = OnceLock::new();

/// Locks the hub, recovering from a poisoned lock.
///
/// A panic in one callback must not take the feature down for the rest of the
/// server's uptime: the state behind the lock is plain data, and the worst a
/// recovered lock can do is serve a catalog that was mid-update.
fn lock() -> MutexGuard<'static, Hub> {
    match HUB.get_or_init(|| Mutex::new(Hub::unconfigured())).lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Why there is no frame to send.
enum Action {
    /// Nothing to answer.
    None,
    /// Frames for the player who sent the payload, plus the minor to encode
    /// them at.
    Reply { frames: Vec<Outbound>, minor: i32 },
    /// A `RESULT` for the author, a delta for every subscribed peer.
    Mutation(Box<MutationFrames>),
}

/// The frames one committed mutation produces: the author's `RESULT` and, when
/// the catalog moved, the delta for every subscribed peer.
struct MutationFrames {
    result: Outbound,
    minor: i32,
    broadcast: Option<Outbound>,
    recipients: Vec<(Id, i32)>,
}

impl Hub {
    fn unconfigured() -> Self {
        Hub {
            sessions: Sessions::new(),
            service: None,
            settings: Settings {
                enabled: false,
                world_id: String::new(),
                limits: Limits {
                    max_per_world: proto::MAX_SNAPSHOT_WAYPOINTS,
                    max_per_player: 64,
                    mutations_per_minute: 30,
                },
                access: AccessPolicy::OwnerManaged,
            },
            counters: Counters::default(),
            data_folder: String::new(),
            owner_instance_id: None,
            refresh_status: false,
            pending_deltas: Vec::new(),
        }
    }

    fn configured(&self) -> bool {
        self.service.is_some()
    }

    /// A status frame for a peer.
    fn status(&self, operator: bool, minor: i32, supported: bool) -> Outbound {
        let (revision, owner_allowed) = match &self.service {
            Some(service) => (
                service.snapshot().revision,
                service.access_policy() == AccessPolicy::OwnerManaged,
            ),
            None => (0, false),
        };
        Outbound::Status {
            major: proto::PROTO_MAJOR,
            minor,
            supported,
            enabled: supported && self.settings.enabled,
            operator,
            world_id: self.settings.world_id.clone(),
            revision,
            max_world: self.settings.limits.max_per_world as i32,
            max_player: self.settings.limits.max_per_player as i32,
            owner_management_allowed: owner_allowed,
        }
    }

    /// Every subscribed peer with the minor its frames must be written at.
    fn recipients(&self) -> Vec<(Id, i32)> {
        self.sessions
            .subscribed()
            .into_iter()
            .filter_map(|key| {
                let session = self.sessions.get(key)?;
                Some((key, session.effective_minor()))
            })
            .collect()
    }

    fn session_minor(&self, key: Id) -> i32 {
        self.sessions
            .get(key)
            .map_or(0, session::Session::effective_minor)
    }

    fn session_compatible(&self, key: Id) -> bool {
        self.sessions
            .get(key)
            .is_some_and(session::Session::is_compatible)
    }

    fn charge(&mut self, key: Id, now: i64) -> bool {
        match self.sessions.get_mut(key) {
            Some(session) => session.charge_control(now),
            None => false,
        }
    }

    fn set_operator(&mut self, key: Id, operator: bool) {
        if let Some(session) = self.sessions.get_mut(key) {
            session.set_operator(operator);
        }
    }

    fn set_subscribed(&mut self, key: Id, subscribed: bool) {
        if let Some(session) = self.sessions.get_mut(key) {
            session.set_subscribed(subscribed);
        }
    }

    /// Handles one decoded payload.
    fn handle(&mut self, peer: &Peer, payload: &[u8], now: i64) -> Action {
        let key = peer.id;
        let minor = self.session_minor(key);
        let message = match proto::decode(payload, minor) {
            Ok(message) => message,
            Err(error) => {
                self.note_malformed(key, payload.len(), &error.0);
                return Action::None;
            }
        };

        match message {
            Inbound::Hello { major, minor } => {
                let Some(negotiated) = self.sessions.hello(key, major, minor, peer.operator, now)
                else {
                    // Either muted, or out of control-request budget. Both mean
                    // "answer nothing"; the client times out on its own.
                    return Action::None;
                };
                self.counters.hellos += 1;
                // A peer on another protocol major gets an explicit "unsupported"
                // status rather than silence: that is what the client reads to
                // stop retrying and show the feature as unavailable.
                let supported = self.session_compatible(key);
                Action::Reply {
                    frames: vec![self.status(peer.operator, negotiated, supported)],
                    minor: negotiated,
                }
            }
            Inbound::Subscribe => {
                if !self.session_compatible(key) || !self.charge(key, now) {
                    return Action::None;
                }
                self.set_operator(key, peer.operator);
                if !self.settings.enabled {
                    // Disabling invalidates the subscription; the status frame is
                    // what tells the client why it is being served nothing.
                    self.set_subscribed(key, false);
                    return Action::Reply {
                        frames: vec![self.status(peer.operator, minor, true)],
                        minor,
                    };
                }
                let snapshot = self
                    .service
                    .as_ref()
                    .map(Service::snapshot)
                    .unwrap_or_else(Snapshot::empty);
                self.set_subscribed(key, true);
                self.counters.subscribes += 1;
                Action::Reply {
                    frames: vec![Outbound::Snapshot {
                        revision: snapshot.revision,
                        operator: peer.operator,
                        waypoints: snapshot.waypoints,
                    }],
                    minor,
                }
            }
            Inbound::Create(request) => {
                if !self.session_compatible(key) {
                    return Action::None;
                }
                if !self.settings.enabled {
                    return self.disabled(peer, request.operation_id, minor, now);
                }
                let actor = actor_of(peer);
                let result = match self.service.as_mut() {
                    Some(service) => service.create(&actor, &request),
                    None => return Action::None,
                };
                self.finish_mutation(&peer.name, result, minor)
            }
            Inbound::Update(request) => {
                if !self.session_compatible(key) {
                    return Action::None;
                }
                if !self.settings.enabled {
                    return self.disabled(peer, request.operation_id, minor, now);
                }
                let actor = actor_of(peer);
                let result = match self.service.as_mut() {
                    Some(service) => service.update(&actor, &request),
                    None => return Action::None,
                };
                self.finish_mutation(&peer.name, result, minor)
            }
            Inbound::Delete(request) => {
                if !self.session_compatible(key) {
                    return Action::None;
                }
                if !self.settings.enabled {
                    return self.disabled(peer, request.operation_id, minor, now);
                }
                let actor = actor_of(peer);
                let result = match self.service.as_mut() {
                    Some(service) => service.delete(&actor, &request),
                    None => return Action::None,
                };
                self.finish_mutation(&peer.name, result, minor)
            }
            Inbound::Lock(request) => {
                if !self.session_compatible(key) {
                    return Action::None;
                }
                if !self.settings.enabled {
                    return self.disabled(peer, request.operation_id, minor, now);
                }
                // Locking was removed with server markers. The reference server
                // answers every lock request with INVALID_REQUEST, which is what
                // keeps a client from retrying it.
                Action::Reply {
                    frames: vec![Outbound::Result {
                        operation_id: request.operation_id,
                        status: proto::RESULT_STATUS_REJECTED,
                        error: proto::RESULT_ERROR_INVALID_REQUEST,
                    }],
                    minor,
                }
            }
        }
    }

    /// The answer to a mutation while the feature is switched off.
    fn disabled(&mut self, peer: &Peer, operation_id: Id, minor: i32, now: i64) -> Action {
        if !self.charge(peer.id, now) {
            return Action::Reply {
                frames: vec![Outbound::Result {
                    operation_id,
                    status: proto::RESULT_STATUS_REJECTED,
                    error: proto::RESULT_ERROR_RATE_LIMITED,
                }],
                minor,
            };
        }
        self.set_operator(peer.id, peer.operator);
        Action::Reply {
            frames: vec![
                Outbound::Result {
                    operation_id,
                    status: proto::RESULT_STATUS_REJECTED,
                    error: proto::RESULT_ERROR_DISABLED,
                },
                self.status(peer.operator, minor, true),
            ],
            minor,
        }
    }

    /// Turns a service result into the frames it produces.
    fn finish_mutation(&mut self, actor_name: &str, result: MutationResult, minor: i32) -> Action {
        if result.applied() {
            self.counters.applied += 1;
        } else {
            self.counters.rejected += 1;
        }
        // A replayed operation already produced its delta the first time, and a
        // rejected one produced none. Only a fresh applied mutation moves the
        // subscribers' caches.
        let broadcast = if result.applied() && !result.replayed {
            delta_to_outbound(&result.delta)
        } else {
            None
        };
        debug!(
            "[confluxmap] shared-waypoint: mutation by {} -> status {} error {}",
            actor_name,
            result.status_code(),
            result.error_code(minor)
        );
        let recipients = match broadcast {
            Some(_) => {
                self.counters.broadcasts += 1;
                self.recipients()
            }
            None => Vec::new(),
        };
        Action::Mutation(Box::new(MutationFrames {
            result: Outbound::Result {
                operation_id: result.operation_id,
                status: result.status_code(),
                error: result.error_code(minor),
            },
            minor,
            broadcast,
            recipients,
        }))
    }

    /// Records a malformed payload, muting a connection that keeps sending them.
    fn note_malformed(&mut self, key: Id, bytes: usize, reason: &str) {
        self.counters.malformed += 1;
        let outcome = match self.sessions.get_mut(key) {
            Some(session) => session.record_malformed(),
            // No session: the tracked-session cap is full. Nothing to strike.
            None => {
                warn!(
                    "[confluxmap] shared-waypoint: dropped a malformed {bytes}-byte payload \
                     from an untracked session ({reason})"
                );
                return;
            }
        };
        warn!(
            "[confluxmap] shared-waypoint: dropped a malformed {bytes}-byte payload \
             (strike {}/{}, reason={reason})",
            outcome.strikes,
            session::MAX_MALFORMED_STRIKES
        );
        if outcome.newly_muted {
            self.counters.muted += 1;
            warn!("[confluxmap] shared-waypoint: muting malformed payloads until disconnect");
        }
    }

    /// A multi-line dump for `/cfm waypoints`.
    fn describe(&self) -> String {
        let mut out = String::from("== confluxmap shared waypoints ==\n");
        let _ = writeln!(out, "enabled        = {}", self.settings.enabled);
        let _ = writeln!(out, "channel        = {}", proto::CHANNEL_ID);
        let _ = writeln!(out, "world_id       = {}", self.settings.world_id);
        match &self.service {
            Some(service) => {
                let snapshot = service.snapshot();
                let _ = writeln!(out, "revision       = {}", snapshot.revision);
                let _ = writeln!(
                    out,
                    "waypoints      = {} of {} (per player {})",
                    snapshot.waypoints.len(),
                    self.settings.limits.max_per_world,
                    self.settings.limits.max_per_player
                );
            }
            None => out.push_str("catalog        = not loaded yet\n"),
        }
        let _ = writeln!(
            out,
            "access         = {}",
            match self.settings.access {
                AccessPolicy::OperatorOnly => "operators only",
                AccessPolicy::OwnerManaged => "owners manage their own",
            }
        );
        let _ = writeln!(
            out,
            "rate limit     = {} mutations/minute per player",
            self.settings.limits.mutations_per_minute
        );
        let _ = writeln!(
            out,
            "storage        = {}/{}",
            self.data_folder,
            persist::FILE_NAME
        );
        let _ = writeln!(
            out,
            "sessions       = {} tracked, {} compatible, {} subscribed",
            self.sessions.len(),
            self.sessions.compatible().len(),
            self.sessions.subscribed().len()
        );
        let _ = writeln!(
            out,
            "traffic        = {} hellos, {} subscribes, {} applied, {} rejected, {} broadcasts",
            self.counters.hellos,
            self.counters.subscribes,
            self.counters.applied,
            self.counters.rejected,
            self.counters.broadcasts
        );
        let players = match &self.service {
            Some(service) => service.tracked_player_count(),
            None => 0,
        };
        let _ = writeln!(out, "players        = {players} with cached operations");
        let _ = writeln!(
            out,
            "malformed      = {} dropped, {} muted",
            self.counters.malformed, self.counters.muted
        );
        let _ = writeln!(out, "encode errors  = {}", self.counters.encode_failures);
        out
    }

    /// One page of the catalog, for `/cfm waypoints list`.
    fn list_page(&self, page: u32) -> String {
        let Some(service) = &self.service else {
            return "the catalog is not loaded yet\n".to_string();
        };
        let snapshot = service.snapshot();
        if snapshot.waypoints.is_empty() {
            return "no shared waypoints\n".to_string();
        }
        let pages = snapshot.waypoints.len().div_ceil(PAGE_SIZE).max(1);
        let page = page.clamp(1, pages as u32) as usize;
        let start = (page - 1) * PAGE_SIZE;
        let mut out = format!(
            "== confluxmap shared waypoints (page {page}/{pages}, revision {}) ==\n",
            snapshot.revision
        );
        for waypoint in snapshot.waypoints.iter().skip(start).take(PAGE_SIZE) {
            let _ = writeln!(
                out,
                "[{}] {} by {} @ {} ({:.1}, {:.1}, {:.1}) rev {}",
                id_prefix(waypoint),
                waypoint.name,
                waypoint.publisher_name,
                waypoint.dimension_id,
                waypoint.x,
                waypoint.y,
                waypoint.z,
                waypoint.revision
            );
        }
        if page < pages {
            let _ = writeln!(out, "next: /cfm waypoints list {}", page + 1);
        }
        out
    }

    /// Replaces the catalog with an empty one, one revision at a time.
    ///
    /// This is an operator action, so it deliberately bypasses the per-player
    /// quota and rate limit that a player's own mutations pass through. Each
    /// removed point still produces its own `REMOVE` delta at its own revision,
    /// because a subscribed client applies deltas strictly one revision at a
    /// time and would resynchronise from scratch if they arrived as a gap.
    fn clear(&mut self) -> Result<usize, String> {
        let Some(service) = self.service.as_ref() else {
            return Ok(0);
        };
        let snapshot = service.snapshot();
        if snapshot.waypoints.is_empty() {
            return Ok(0);
        }
        let removed: Vec<Id> = snapshot
            .waypoints
            .iter()
            .map(|waypoint| waypoint.id)
            .collect();
        let next = snapshot
            .revision
            .checked_add(removed.len() as i64)
            .ok_or_else(|| "the catalog revision cannot be advanced that far".to_string())?;
        let persistence = Persistence::new(&self.data_folder, self.owner_instance_id.as_deref());
        let empty = Snapshot {
            revision: next,
            waypoints: Vec::new(),
        };
        // Write first, then install: a failed write must leave the live catalog
        // exactly as it was.
        persistence
            .save(&empty)
            .map_err(|error| format!("could not write {}: {error}", persistence.path()))?;
        let store = Store::new(empty)
            .map_err(|error| format!("could not rebuild the catalog: {error:?}"))?;
        let limits = self.settings.limits;
        let access = self.settings.access;
        self.service = Some(Service::new(store, persistence, limits, access));
        self.pending_deltas = removed
            .iter()
            .enumerate()
            .filter_map(|(index, id)| {
                delta_to_outbound(&Delta::remove(*id, snapshot.revision + 1 + index as i64))
            })
            .collect();
        Ok(removed.len())
    }
}

fn actor_of(peer: &Peer) -> Actor {
    Actor {
        id: peer.id,
        name: peer.name.clone(),
        operator: peer.operator,
    }
}

fn delta_to_outbound(delta: &Delta) -> Option<Outbound> {
    match delta.kind {
        DeltaKind::Upsert => delta.waypoint.clone().map(|waypoint| Outbound::Upsert {
            revision: delta.revision,
            waypoint,
        }),
        DeltaKind::Remove => delta.removed_id.map(|id| Outbound::Remove {
            revision: delta.revision,
            id,
        }),
        DeltaKind::Noop => None,
    }
}

/// The first eight hex digits of a waypoint id, as the command output shows it.
fn id_prefix(waypoint: &Waypoint) -> String {
    let text = waypoint.id.text();
    text.chars().take(8).collect()
}

/// The protocol identity of a player, as the session layer needs it.
fn peer_of(player: &Player) -> Peer {
    Peer {
        id: Peer::key(player.get_id()),
        name: player.get_name(),
        operator: Peer::is_operator(permission_level(player)),
    }
}

/// A player's op level, as a number.
///
/// The WIT enum is a plain fieldless enum, so the discriminant is the level; the
/// generated type is not nameable from here, which is why this is a cast rather
/// than a comparison.
fn permission_level(player: &Player) -> u8 {
    player.get_permission_level() as u8
}

/// Applies the configuration, loading the catalog the first time.
///
/// Returns anything worth logging. A later call updates settings in place and
/// deliberately does **not** re-read the file: an operator running
/// `/cfm reload` wants the new flags, not a rollback of everything players have
/// published since the last write.
pub fn configure(data_folder: &str, config: &Config, world_id: &str) -> Vec<String> {
    let limits = Limits {
        max_per_world: config.max_waypoints_per_world as usize,
        max_per_player: config.max_waypoints_per_player as usize,
        mutations_per_minute: config.waypoint_mutations_per_minute,
    };
    let access = if config.allow_non_operator_waypoint_management {
        AccessPolicy::OwnerManaged
    } else {
        AccessPolicy::OperatorOnly
    };
    let mut hub = lock();
    let mut warnings = Vec::new();

    if hub.configured() {
        let settings_changed = hub.settings.enabled != config.share_waypoints
            || hub.settings.world_id != world_id
            || hub.settings.access != access
            || hub.settings.limits != limits;
        if hub.settings.enabled && !config.share_waypoints {
            // A disable has to invalidate subscriptions, or a subscribed client
            // would keep being served deltas for a feature that is off.
            hub.sessions.clear_subscriptions();
        }
        hub.settings = Settings {
            enabled: config.share_waypoints,
            world_id: world_id.to_string(),
            limits,
            access,
        };
        if settings_changed {
            hub.refresh_status = true;
        }
        return warnings;
    }

    let owner = crate::state::instance_id().map(str::to_string);
    let persistence = Persistence::new(data_folder, owner.as_deref());
    let snapshot = match persistence.load() {
        persist::Loaded::Ready {
            snapshot,
            warnings: load_warnings,
        } => {
            warnings.extend(load_warnings);
            snapshot
        }
        persist::Loaded::UnsupportedSchema { version } => {
            // A newer build wrote this file. Leave it alone and start with the
            // feature off rather than quarantining data that is not corrupt.
            warnings.push(format!(
                "{} uses schema {version}, which is newer than this build understands ({})",
                persistence.path(),
                persist::SCHEMA_VERSION
            ));
            warnings.push(
                "shared waypoints are disabled until the file is handled by a matching build"
                    .to_string(),
            );
            hub.data_folder = data_folder.to_string();
            hub.owner_instance_id = owner;
            hub.settings = Settings {
                enabled: false,
                world_id: world_id.to_string(),
                limits,
                access,
            };
            return warnings;
        }
    };
    let (snapshot, sanitize_warnings) = service::sanitize_loaded(snapshot);
    warnings.extend(sanitize_warnings);

    match Store::new(snapshot) {
        Ok(store) => {
            hub.service = Some(Service::new(store, persistence, limits, access));
        }
        Err(error) => {
            warnings.push(format!(
                "the stored catalog is inconsistent ({error:?}); starting empty"
            ));
            let persistence = Persistence::new(data_folder, owner.as_deref());
            hub.service = Some(Service::new(Store::empty(), persistence, limits, access));
        }
    }
    hub.data_folder = data_folder.to_string();
    hub.owner_instance_id = owner;
    hub.settings = Settings {
        enabled: config.share_waypoints,
        world_id: world_id.to_string(),
        limits,
        access,
    };
    hub.refresh_status = true;
    warnings
}

/// Handles one payload on [`CHANNEL_ID`].
pub fn on_payload(server: &Server, player: &Player, data: &[u8]) {
    let peer = peer_of(player);
    let action = lock().handle(&peer, data, now_ms());

    match action {
        Action::None => {}
        Action::Reply { frames, minor } => {
            for frame in &frames {
                send_frame(player, frame, minor);
            }
        }
        Action::Mutation(frames) => {
            send_frame(player, &frames.result, frames.minor);
            let Some(delta) = frames.broadcast else {
                return;
            };
            for candidate in server.get_all_players() {
                let key = Peer::key(candidate.get_id());
                let Some((_, candidate_minor)) = frames
                    .recipients
                    .iter()
                    .find(|(recipient, _)| *recipient == key)
                else {
                    continue;
                };
                send_frame(&candidate, &delta, *candidate_minor);
            }
        }
    }
}

/// Runs every second: refreshes operator status and flushes anything queued.
pub fn on_tick(server: &Server) {
    let players = server.get_all_players();
    let levels: Vec<(Id, bool)> = players
        .iter()
        .map(|player| {
            (
                Peer::key(player.get_id()),
                Peer::is_operator(permission_level(player)),
            )
        })
        .collect();

    let (statuses, deltas, subscribers) = {
        let mut hub = lock();
        let mut targets: Vec<Id> = Vec::new();
        if hub.refresh_status {
            hub.refresh_status = false;
            targets.extend(hub.sessions.compatible());
        }
        // An operator change is only visible to a client through a new status
        // frame, so a grant or revocation is pushed rather than polled for.
        for (key, operator) in &levels {
            if hub.sessions.refresh_operator(*key, *operator) {
                targets.push(*key);
            }
        }
        targets.sort_unstable();
        targets.dedup();

        let statuses: Vec<(Id, i32, Outbound)> = targets
            .into_iter()
            .filter_map(|key| {
                let session = hub.sessions.get(key)?;
                if !session.is_compatible() {
                    return None;
                }
                let minor = session.effective_minor();
                let supported = hub.service.is_some();
                Some((
                    key,
                    minor,
                    hub.status(session.is_operator(), minor, supported),
                ))
            })
            .collect();
        // A queued delta is addressed to every subscriber, read now so the
        // sends below need no lock.
        let subscribers = if hub.pending_deltas.is_empty() {
            Vec::new()
        } else {
            hub.recipients()
        };
        (
            statuses,
            std::mem::take(&mut hub.pending_deltas),
            subscribers,
        )
    };

    for (key, minor, frame) in &statuses {
        if let Some(player) = players.iter().find(|p| Peer::key(p.get_id()) == *key) {
            send_frame(player, frame, *minor);
        }
    }
    for delta in &deltas {
        for (key, minor) in &subscribers {
            if let Some(player) = players.iter().find(|p| Peer::key(p.get_id()) == *key) {
                send_frame(player, delta, *minor);
            }
        }
    }
}

/// Forgets every session.
///
/// Called when the plugin unloads: a session is protocol state for a connection
/// that is still open, and keeping it would let a reloaded plugin answer a peer
/// whose negotiated minor it no longer knows.
pub fn on_unload() {
    lock().sessions.clear();
}

/// Drops a player's session.
pub fn on_leave(player: &Player) {
    lock().sessions.remove(Peer::key(player.get_id()));
}

/// A multi-line status dump for `/cfm waypoints`.
pub fn describe() -> String {
    lock().describe()
}

/// One page of the catalog for `/cfm waypoints list`.
pub fn list_page(page: u32) -> String {
    lock().list_page(page)
}

/// Empties the catalog, returning how many points were removed.
pub fn clear() -> Result<usize, String> {
    lock().clear()
}

/// Sends one frame, logging an encoding failure rather than dropping it silently.
fn send_frame(player: &Player, message: &Outbound, minor: i32) {
    let Some(java) = player.as_java() else {
        return;
    };
    let bytes = match proto::encode(message, minor) {
        Ok(bytes) => bytes,
        Err(error) => {
            lock().counters.encode_failures += 1;
            warn!(
                "[confluxmap] shared-waypoint: could not encode {}: {error}",
                message.kind()
            );
            return;
        }
    };
    if bytes.len() > MAX_S2C_PAYLOAD {
        lock().counters.encode_failures += 1;
        warn!(
            "[confluxmap] shared-waypoint: {} is {} bytes, over the {MAX_S2C_PAYLOAD}-byte cap",
            message.kind(),
            bytes.len()
        );
        return;
    }
    java.send_custom_payload(proto::CHANNEL_ID, &bytes);
    if message.kind() == "SNAPSHOT" {
        info!(
            "[confluxmap] shared-waypoint: sent a {} frame of {} bytes to {}",
            message.kind(),
            bytes.len(),
            player.get_name()
        );
    }
}
