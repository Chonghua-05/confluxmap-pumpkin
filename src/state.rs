//! Process-wide plugin state: the live configuration and handshake counters.
//!
//! Everything is behind a lock or an atomic because the host may re-enter the
//! plugin (an event callback can arrive while a command handler is still on the
//! stack). The rule throughout is: **take a snapshot, release the lock, then
//! make host calls**. Never hold a lock across `send_custom_payload`,
//! `get_all_players`, or any other host function.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, RwLock};

use pumpkin_plugin_api::Server;

use crate::config::Config;

/// How many recent handshakes to keep for `/cfm hello`.
const RECENT_CAPACITY: usize = 16;

/// How many distinct channel names to remember for diagnostics.
const CHANNEL_CAPACITY: usize = 32;

static CONFIG: OnceLock<RwLock<Config>> = OnceLock::new();

/// The sandbox-visible path of the plugin's data folder, kept so `/cfm reload`
/// can re-read `config.toml` without being handed a [`Context`](pumpkin_plugin_api::Context).
static DATA_FOLDER: OnceLock<String> = OnceLock::new();

/// Remembers the data folder the host reported at load time.
pub fn set_data_folder(path: String) {
    // A second `on_load` without an intervening unload cannot happen, and losing
    // this race would only mean reload keeps using the first path, which is the
    // same one.
    let _ = DATA_FOLDER.set(path);
}

/// The data folder, once the plugin has been loaded.
pub fn data_folder() -> Option<&'static str> {
    DATA_FOLDER.get().map(String::as_str)
}

/// This server's instance identifier, chosen once in `on_load`.
///
/// It is global rather than threaded through every call because a handshake
/// needs it per player while the value itself is fixed for the process: it names
/// this server in the client's cache, so it must stay identical across every
/// player of a session.
static INSTANCE_ID: OnceLock<String> = OnceLock::new();

/// Remembers the instance id chosen at load time.
pub fn set_instance_id(id: String) {
    // Same reasoning as `DATA_FOLDER`: a second `on_load` cannot run without an
    // intervening unload, so losing this race only keeps the first (identical) id.
    let _ = INSTANCE_ID.set(id);
}

/// The instance id, once the plugin has been loaded.
pub fn instance_id() -> Option<&'static str> {
    INSTANCE_ID.get().map(String::as_str)
}

static HELLOS_SEEN: AtomicU64 = AtomicU64::new(0);
static POLICIES_SENT: AtomicU64 = AtomicU64::new(0);
static SEND_FAILURES: AtomicU64 = AtomicU64::new(0);
static CHANNELS_REGISTERED: AtomicU64 = AtomicU64::new(0);
static LAST_POLICY: OnceLock<Mutex<Option<Vec<u8>>>> = OnceLock::new();
static RECENT: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();
static CHANNELS_SEEN: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();

/// Remembers a non-companion channel a client registered.
///
/// Deduplicated and bounded: the point is to answer "what did this client
/// register instead of us?" at a glance, not to keep a log.
pub fn note_channel(channel: &str) {
    let cell = CHANNELS_SEEN.get_or_init(|| Mutex::new(VecDeque::new()));
    let mut guard = match cell.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if guard.iter().any(|seen| seen == channel) {
        return;
    }
    if guard.len() == CHANNEL_CAPACITY {
        guard.pop_front();
    }
    guard.push_back(channel.to_string());
}

/// The distinct non-companion channels seen so far, oldest first.
pub fn recent_channels() -> Vec<String> {
    let cell = CHANNELS_SEEN.get_or_init(|| Mutex::new(VecDeque::new()));
    let guard = match cell.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.iter().cloned().collect()
}

/// Players already given this session's late channel re-announcement.
static LATE_ANNOUNCED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

/// Claims the single late re-announcement allowed per player session.
///
/// Returns `true` for the first caller only. A modded client registers dozens of
/// loader channels back to back; without this the safety-net announcement would
/// fire once per channel, costing ~40 redundant packets and log lines per join.
pub fn claim_late_announce(player: &str) -> bool {
    let cell = LATE_ANNOUNCED.get_or_init(|| Mutex::new(HashSet::new()));
    let mut guard = match cell.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.insert(player.to_string())
}

/// Forgets a player's claim, so their next session gets a fresh one.
pub fn reset_late_announce(player: &str) {
    let cell = LATE_ANNOUNCED.get_or_init(|| Mutex::new(HashSet::new()));
    let mut guard = match cell.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.remove(player);
}

fn config_cell() -> &'static RwLock<Config> {
    CONFIG.get_or_init(|| RwLock::new(Config::default()))
}

/// Installs `config` as the live configuration.
pub fn set_config(config: Config) {
    let cell = config_cell();
    match cell.write() {
        Ok(mut guard) => *guard = config,
        Err(poisoned) => *poisoned.into_inner() = config,
    }
}

/// Clones the live configuration out of the lock.
///
/// Callers hold the snapshot for the duration of a handshake rather than holding
/// the read lock, so a concurrent `/cfm reload` cannot deadlock against a host
/// call made mid-handshake.
pub fn config_snapshot() -> Config {
    let cell = config_cell();
    match cell.read() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

/// Records that a HELLO frame was decoded.
pub fn count_hello() -> u64 {
    HELLOS_SEEN.fetch_add(1, Ordering::Relaxed) + 1
}

/// Records that a policy was handed to the transport layer.
pub fn count_policy_sent() -> u64 {
    POLICIES_SENT.fetch_add(1, Ordering::Relaxed) + 1
}

/// Records that the transport rejected the reply (a Bedrock client, or a Java
/// player that disconnected mid-handshake).
pub fn count_send_failure() -> u64 {
    SEND_FAILURES.fetch_add(1, Ordering::Relaxed) + 1
}

/// Records a channel registration from a client's mod loader.
pub fn count_channel_registered() -> u64 {
    CHANNELS_REGISTERED.fetch_add(1, Ordering::Relaxed) + 1
}

/// Remembers the exact bytes of the last policy sent, for `/cfm hello`.
pub fn remember_policy(bytes: &[u8]) {
    let cell = LAST_POLICY.get_or_init(|| Mutex::new(None));
    match cell.lock() {
        Ok(mut guard) => *guard = Some(bytes.to_vec()),
        Err(poisoned) => *poisoned.into_inner() = Some(bytes.to_vec()),
    }
}

/// The exact bytes of the last policy sent, if any.
pub fn last_policy() -> Option<Vec<u8>> {
    let cell = LAST_POLICY.get_or_init(|| Mutex::new(None));
    match cell.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

/// Appends a one-line handshake summary, keeping only the most recent entries.
pub fn remember_handshake(line: String) {
    let cell = RECENT.get_or_init(|| Mutex::new(VecDeque::new()));
    let mut guard = match cell.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if guard.len() == RECENT_CAPACITY {
        guard.pop_front();
    }
    guard.push_back(line);
}

/// The most recent handshake summaries, oldest first.
pub fn recent_handshakes() -> Vec<String> {
    let cell = RECENT.get_or_init(|| Mutex::new(VecDeque::new()));
    let guard = match cell.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.iter().cloned().collect()
}

/// A snapshot of the handshake counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// HELLO frames successfully decoded.
    pub hellos: u64,
    /// Policies handed to the transport.
    pub policies_sent: u64,
    /// Policies the transport refused to send.
    pub send_failures: u64,
    /// Channel registrations observed from clients.
    pub channels_registered: u64,
}

/// Reads every counter at once.
pub fn counters() -> Counters {
    Counters {
        hellos: HELLOS_SEEN.load(Ordering::Relaxed),
        policies_sent: POLICIES_SENT.load(Ordering::Relaxed),
        send_failures: SEND_FAILURES.load(Ordering::Relaxed),
        channels_registered: CHANNELS_REGISTERED.load(Ordering::Relaxed),
    }
}

/// Formats a byte slice as lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// The running server's version string, e.g. `0.1.0-dev+26.2-26.45`.
///
/// `pumpkin-version` is not gated behind `sys.info`, so this needs no extra
/// permission; the other `sys-info` fields (CPU, RAM, OS) do, and we never read
/// them.
pub fn server_version(server: &Server) -> Option<String> {
    let version = server.get_sys_info().pumpkin_version;
    if version.is_empty() {
        None
    } else {
        Some(version)
    }
}
