//! Identity: random UUIDs, and the id that names *this server instance*.
//!
//! The reference companion sends capability-aware clients an `instanceId`
//! alongside the policy. It is a different thing from the policy's `worldId`:
//!
//! * `worldId` is stored inside the world save, so it travels with a copied
//!   world - two servers sharing a synced world advertise the *same* one, and so
//!   does every dimension of one server's overworld-derived cache namespace.
//! * `instanceId` lives beside the companion's *configuration*, so each server
//!   keeps its own even when the world is identical.
//!
//! The client uses the instance id as the storage namespace for everything it
//! caches against that server. Without it, two servers behind a Velocity proxy
//! that happen to run the same world would overwrite each other's map data.
//!
//! # Where the randomness comes from
//!
//! The plugin API exposes no random source, and the WIT `uuid` interface's
//! `generate` function is not re-exported by the Rust bindings. `std` already
//! has the entropy this needs and the plugin already depends on it: `HashMap`'s
//! `RandomState` is seeded from the OS RNG once per process, and each
//! construction bumps one of its two keys. Two values drawn here therefore
//! differ from each other within a run, and differ across runs. The id is
//! persisted on first use, so it only has to be drawn once.

use std::collections::hash_map::RandomState;
use std::fmt;
use std::fmt::Write as _;
use std::fs;
use std::hash::{BuildHasher, Hasher};
use std::io::ErrorKind;

use pumpkin_plugin_api::mobs::Uuid;

use crate::json;

/// The file holding this server's instance id, beside the configuration.
pub const INSTANCE_FILE: &str = "server_instance.json";

/// An identity this plugin can actually use.
///
/// The plugin API's `Uuid` is a WIT record with no `PartialEq` or `Hash`, so it
/// can be moved around but not compared or used as a map key - and the waypoint
/// protocol needs both constantly: sessions are keyed by player, and the catalog
/// by waypoint id. `Id` is that same 128-bit value in a form Rust can compare,
/// converted only where it crosses the API boundary.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Id {
    /// The most significant 64 bits.
    pub high: u64,
    /// The least significant 64 bits.
    pub low: u64,
}

impl Id {
    /// From the API's UUID.
    pub fn of(uuid: Uuid) -> Self {
        Id {
            high: uuid.high,
            low: uuid.low,
        }
    }

    /// To the API's UUID.
    pub fn uuid(self) -> Uuid {
        Uuid {
            high: self.high,
            low: self.low,
        }
    }

    /// A fresh random id.
    pub fn random() -> Self {
        random_id()
    }

    /// Parses the canonical `8-4-4-4-12` form.
    pub fn parse(text: &str) -> Option<Self> {
        parse_uuid(text)
    }

    /// The canonical text form.
    pub fn text(self) -> String {
        self.uuid().to_string()
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.uuid())
    }
}

impl fmt::Debug for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.uuid())
    }
}

/// Draws a fresh identity.
///
/// See the module docs: this is `std`'s own process entropy, not a
/// cryptographically strong generator. It names a cache namespace, which is all
/// it is asked to do.
pub fn random_id() -> Id {
    let mut halves = [0u64; 2];
    for half in &mut halves {
        *half = RandomState::new().build_hasher().finish();
    }
    Id {
        high: halves[0],
        low: halves[1],
    }
}

/// Parses the standard `8-4-4-4-12` hexadecimal form.
///
/// Returns `None` for anything else, including a well-formed UUID carrying a
/// version or variant this plugin would not have written: a hand-edited
/// identity file is treated as unreadable rather than trusted.
pub fn parse_uuid(text: &str) -> Option<Id> {
    let bytes = text.as_bytes();
    if bytes.len() != 36 {
        return None;
    }
    let mut digits = [0u8; 32];
    let mut count = 0;
    for (index, byte) in bytes.iter().enumerate() {
        match byte {
            b'-' if index == 8 || index == 13 || index == 18 || index == 23 => {}
            b'-' => return None,
            digit => {
                if count == 32 {
                    return None;
                }
                digits[count] = match digit {
                    b'0'..=b'9' => digit - b'0',
                    b'a'..=b'f' => digit - b'a' + 10,
                    b'A'..=b'F' => digit - b'A' + 10,
                    _ => return None,
                };
                count += 1;
            }
        }
    }
    if count != 32 {
        return None;
    }
    let mut halves = [0u64; 2];
    for (index, nibble) in digits.iter().enumerate() {
        let slot = index / 16;
        halves[slot] = (halves[slot] << 4) | u64::from(*nibble);
    }
    Some(Id {
        high: halves[0],
        low: halves[1],
    })
}

/// This server's instance id, plus anything the operator should know about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instance {
    /// The id advertised to capability-aware clients.
    pub id: Id,
    /// Whether it was generated on this call rather than read back.
    pub created: bool,
    /// A problem worth logging: an unreadable file, or one that could not be
    /// written. Both leave a usable id in memory.
    pub warning: Option<String>,
}

impl Instance {
    /// The canonical string form, which is what goes on the wire.
    pub fn id_string(&self) -> String {
        self.id.to_string()
    }
}

/// Reads this server's instance id, generating and persisting one if needed.
///
/// An unreadable file is replaced rather than reported as a failure: the
/// identity it named is already lost, and refusing to advertise one would be a
/// worse outcome than a fresh namespace. That mirrors the reference
/// implementation's `UuidFileStore.loadOrCreate`.
pub fn load_or_create_instance(data_folder: &str) -> Instance {
    let path = format!("{data_folder}/{INSTANCE_FILE}");
    match fs::read_to_string(&path) {
        Ok(text) => match parse_instance(&text) {
            Some(id) => Instance {
                id,
                created: false,
                warning: None,
            },
            None => {
                let id = random_id();
                let mut note = format!(
                    "{INSTANCE_FILE} does not hold a usable instance id; wrote a fresh one"
                );
                if let Err(error) = write_instance(&path, id) {
                    let _ = write!(note, "; it could not be persisted: {error}");
                }
                Instance {
                    id,
                    created: true,
                    warning: Some(note),
                }
            }
        },
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let id = random_id();
            let warning = write_instance(&path, id).err().map(|error| {
                format!("could not persist {INSTANCE_FILE}: {error}; the id will change on restart")
            });
            Instance {
                id,
                created: true,
                warning,
            }
        }
        Err(error) => {
            let id = random_id();
            Instance {
                id,
                created: true,
                warning: Some(format!(
                    "could not read {INSTANCE_FILE}: {error}; using an id that will change on restart"
                )),
            }
        }
    }
}

/// Extracts `{"uuid": "..."}` from a document, or `None` if it is not that.
fn parse_instance(text: &str) -> Option<Id> {
    let value = json::parse(text).ok()?;
    parse_uuid(value.get("uuid")?.as_str()?)
}

/// Writes the identity document, atomically where the platform allows it.
fn write_instance(path: &str, id: Id) -> Result<(), String> {
    let document = format!("{{\n  \"uuid\": \"{id}\"\n}}");
    let temporary = format!("{path}.tmp");
    fs::write(&temporary, document).map_err(|error| error.to_string())?;
    fs::rename(&temporary, path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        error.to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_are_distinct_and_canonical() {
        let first = random_id();
        let second = random_id();
        assert_ne!(
            (first.high, first.low),
            (second.high, second.low),
            "two draws must not collide"
        );
        let text = first.to_string();
        assert_eq!(text.len(), 36);
        assert_eq!(parse_uuid(&text), Some(first), "string form round-trips");
    }

    #[test]
    fn parses_only_canonical_uuid_text() {
        let id = Id {
            high: 0x0011_2233_4455_6677,
            low: 0x8899_aabb_ccdd_eeff,
        };
        assert_eq!(parse_uuid("00112233-4455-6677-8899-aabbccddeeff"), Some(id));
        assert_eq!(parse_uuid("00112233445566778899AABBCCDDEEFF"), None);
        assert_eq!(parse_uuid("00112233-4455-6677-8899-aabbccddeef"), None);
        assert_eq!(parse_uuid("00112233-4455-6677-8899-aabbccddeefg"), None);
        assert_eq!(parse_uuid(""), None);
    }

    #[test]
    fn reads_the_document_shape_the_reference_store_writes() {
        let id = parse_instance("{\"uuid\": \"00112233-4455-6677-8899-aabbccddeeff\"}")
            .expect("canonical document");
        assert_eq!(id.to_string(), "00112233-4455-6677-8899-aabbccddeeff");
        assert_eq!(parse_instance("{\n  \"uuid\": \"not-a-uuid\"\n}"), None);
        assert_eq!(parse_instance("{}"), None);
        assert_eq!(parse_instance(""), None);
    }

    #[test]
    fn the_identity_file_round_trips_and_is_written_once() {
        let folder = std::env::temp_dir().join(format!(
            "cfm-identity-test-{}-{:?}",
            std::process::id(),
            random_id()
        ));
        fs::create_dir_all(&folder).expect("temp dir");
        let folder = folder.to_string_lossy().to_string();

        let first = load_or_create_instance(&folder);
        assert!(first.created, "a missing file means a fresh id");
        assert_eq!(first.warning, None);

        let second = load_or_create_instance(&folder);
        assert!(!second.created, "the written id must be reused");
        assert_eq!(first.id, second.id);

        fs::remove_dir_all(&folder).expect("clean up");
    }

    #[test]
    fn an_unreadable_file_is_replaced_rather_than_fatal() {
        let folder = std::env::temp_dir().join(format!(
            "cfm-identity-bad-{}-{:?}",
            std::process::id(),
            random_id()
        ));
        fs::create_dir_all(&folder).expect("temp dir");
        let path = folder.join(INSTANCE_FILE);
        fs::write(&path, "not json at all").expect("seed a bad file");

        let instance = load_or_create_instance(&folder.to_string_lossy());
        assert!(instance.created);
        assert!(instance.warning.is_some(), "the operator should be told");
        let reread = load_or_create_instance(&folder.to_string_lossy());
        assert_eq!(reread.id, instance.id, "the replacement must be persisted");

        fs::remove_dir_all(&folder).expect("clean up");
    }
}
