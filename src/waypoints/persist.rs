//! Durable storage for the shared-waypoint catalog.
//!
//! The reference companion writes `<worldRoot>/confluxmap/shared_waypoints.json`
//! - inside the world save, because it can read that directory. A Pumpkin plugin
//! cannot: the WASI sandbox preopens exactly one directory, the plugin's own data
//! folder, so the file lives there instead:
//!
//! ```text
//! plugins/data/confluxmap-pumpkin/shared_waypoints.json
//! ```
//!
//! The *document shape* is unchanged, schema version included, so a file brought
//! over from a Paper server still loads. The file's location is the only
//! difference, and it is a forced one.
//!
//! # Failure policy
//!
//! A catalog is not worth refusing to start over, and a half-read one is worse
//! than none: a corrupt file is moved aside as `.bad` and the feature starts
//! empty, while a file a *newer* build wrote is left untouched and the feature
//! stays off for that run. Both are logged loudly. The distinction matters -
//! quarantining a newer file would destroy data that is perfectly good, it is
//! just not readable here.

use std::fs;
use std::io::ErrorKind;

use crate::identity::Id;
use crate::json::{self, Value};
use crate::waypoints::model::{self, MAX_HORIZONTAL_COORDINATE};
use crate::waypoints::proto::{Waypoint, WaypointKind};
use crate::waypoints::store::Snapshot;

/// File name inside the plugin's data folder.
pub const FILE_NAME: &str = "shared_waypoints.json";
/// The document schema this build writes (`SharedWaypointIo.SCHEMA_VERSION`).
pub const SCHEMA_VERSION: i64 = 2;
/// Largest document this build will read.
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// Largest catalog this build will write or read.
const MAX_PERSISTED_WAYPOINTS: usize = 512;

/// What reading the file produced.
#[derive(Clone, Debug, PartialEq)]
pub enum Loaded {
    /// A usable catalog, with anything worth telling the operator.
    Ready {
        /// The catalog.
        snapshot: Snapshot,
        /// Warnings to log.
        warnings: Vec<String>,
    },
    /// A newer build wrote this file. It is left alone.
    UnsupportedSchema {
        /// The version that was found.
        version: i64,
    },
}

/// The catalog file.
#[derive(Clone, Debug)]
pub struct Persistence {
    path: String,
    owner_instance_id: Option<String>,
}

impl Persistence {
    /// Points at the catalog file inside `data_folder`.
    ///
    /// `owner_instance_id` stamps the document with the server that wrote it. It
    /// lives outside the world save, so a file naming a different server is one
    /// an operator copied in, and it is set aside rather than served.
    pub fn new(data_folder: &str, owner_instance_id: Option<&str>) -> Self {
        Persistence {
            path: format!("{data_folder}/{FILE_NAME}"),
            owner_instance_id: owner_instance_id.map(str::to_string),
        }
    }

    /// The absolute-ish path, for operator-facing messages.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Reads the catalog.
    pub fn load(&self) -> Loaded {
        let size = match fs::metadata(&self.path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Loaded::Ready {
                    snapshot: Snapshot::empty(),
                    warnings: Vec::new(),
                };
            }
            Err(error) => {
                return Loaded::Ready {
                    snapshot: Snapshot::empty(),
                    warnings: vec![format!("could not stat {}: {error}", self.path)],
                };
            }
        };
        if size > MAX_FILE_BYTES {
            return self.quarantine(format!("file is {size} bytes, over {MAX_FILE_BYTES}"));
        }
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) => {
                return self.quarantine(format!("file is not readable UTF-8: {error}"));
            }
        };
        let root = match json::parse(&text) {
            Ok(Value::Object(entries)) => Value::Object(entries),
            Ok(_) => return self.quarantine("root is not an object".to_string()),
            Err(error) => return self.quarantine(format!("not valid JSON: {}", error.0)),
        };

        let schema = match root.get("schemaVersion") {
            Some(value) => match value.as_i64() {
                Some(version) => version,
                None => return self.quarantine("schemaVersion is not an integer".to_string()),
            },
            None => {
                return self.quarantine("schemaVersion is missing".to_string());
            }
        };
        if schema > SCHEMA_VERSION {
            return Loaded::UnsupportedSchema { version: schema };
        }
        if schema < 1 {
            return self.quarantine(format!("unsupported schema {schema}"));
        }

        if let Some(owner) = root.get("ownerInstanceId").and_then(Value::as_str)
            && self
                .owner_instance_id
                .as_deref()
                .is_some_and(|mine| mine != owner)
        {
            return self.set_aside_inherited(owner);
        }

        match from_document(&root) {
            Ok(snapshot) => {
                let mut warnings = Vec::new();
                if schema < SCHEMA_VERSION {
                    warnings.push(format!(
                        "migrating {} from schema {schema} to {SCHEMA_VERSION}",
                        self.path
                    ));
                    if let Err(error) = self.save(&snapshot) {
                        warnings.push(format!("migration write failed: {error}"));
                    }
                }
                Loaded::Ready { snapshot, warnings }
            }
            Err(reason) => self.quarantine(reason),
        }
    }

    /// Writes the catalog, replacing the previous file atomically where the
    /// platform allows it.
    pub fn save(&self, snapshot: &Snapshot) -> Result<(), String> {
        if snapshot.waypoints.len() > MAX_PERSISTED_WAYPOINTS {
            return Err(format!(
                "refusing to persist {} waypoints, over {MAX_PERSISTED_WAYPOINTS}",
                snapshot.waypoints.len()
            ));
        }
        let document = to_document(snapshot, self.owner_instance_id.as_deref())?;
        let temporary = format!("{}.tmp", self.path);
        fs::write(&temporary, document).map_err(|error| error.to_string())?;
        fs::rename(&temporary, &self.path).map_err(|error| {
            let _ = fs::remove_file(&temporary);
            error.to_string()
        })
    }

    /// Moves a corrupt file aside and starts empty.
    fn quarantine(&self, reason: String) -> Loaded {
        let aside = format!("{}.bad", self.path);
        let mut warnings = vec![format!("{} is unusable ({reason})", self.path)];
        match fs::rename(&self.path, &aside) {
            Ok(()) => warnings.push(format!("moved it to {aside}; starting an empty catalog")),
            Err(error) => warnings.push(format!(
                "could not move it aside ({error}); starting an empty catalog anyway"
            )),
        }
        Loaded::Ready {
            snapshot: Snapshot::empty(),
            warnings,
        }
    }

    /// Moves a file another server instance wrote aside, and starts empty.
    fn set_aside_inherited(&self, owner: &str) -> Loaded {
        let aside = format!("{}.bak", self.path);
        let mut warnings = vec![format!(
            "shared waypoints at {} belong to server instance {owner}, not this one",
            self.path
        )];
        match fs::rename(&self.path, &aside) {
            Ok(()) => warnings.push(format!("set them aside as {aside}; starting an empty catalog")),
            Err(error) => warnings.push(format!(
                "could not set them aside ({error}); starting an empty catalog anyway"
            )),
        }
        Loaded::Ready {
            snapshot: Snapshot::empty(),
            warnings,
        }
    }
}

/// Serialises a snapshot as the reference document shape.
fn to_document(snapshot: &Snapshot, owner_instance_id: Option<&str>) -> Result<String, String> {
    let mut out = String::with_capacity(256 + snapshot.waypoints.len() * 256);
    out.push_str("{\n  \"schemaVersion\": ");
    out.push_str(&SCHEMA_VERSION.to_string());
    out.push_str(",\n  \"revision\": ");
    out.push_str(&snapshot.revision.to_string());
    out.push_str(",\n  \"ownerInstanceId\": ");
    match owner_instance_id {
        Some(owner) => {
            out.push('"');
            out.push_str(&json::escape(owner));
            out.push('"');
        }
        None => out.push_str("null"),
    }
    out.push_str(",\n  \"waypoints\": [");
    for (index, waypoint) in snapshot.waypoints.iter().enumerate() {
        if !persistable(waypoint, snapshot.revision) {
            return Err(format!(
                "refusing to persist waypoint {}: it is not valid for the current revision",
                waypoint.id
            ));
        }
        if index > 0 {
            out.push(',');
        }
        out.push_str("\n    {\n      \"id\": \"");
        out.push_str(&waypoint.id.to_string());
        out.push_str("\",\n      \"publisherId\": \"");
        out.push_str(&waypoint.publisher_id.to_string());
        out.push_str("\",\n      \"publisherName\": \"");
        out.push_str(&json::escape(&waypoint.publisher_name));
        out.push_str("\",\n      \"name\": \"");
        out.push_str(&json::escape(&waypoint.name));
        out.push_str("\",\n      \"dimensionId\": \"");
        out.push_str(&json::escape(&waypoint.dimension_id));
        out.push_str("\",\n      \"x\": ");
        out.push_str(&json::number(waypoint.x));
        out.push_str(",\n      \"y\": ");
        out.push_str(&json::number(waypoint.y));
        out.push_str(",\n      \"z\": ");
        out.push_str(&json::number(waypoint.z));
        out.push_str(",\n      \"colorArgb\": ");
        out.push_str(&waypoint.color_argb.to_string());
        out.push_str(",\n      \"type\": \"");
        out.push_str(match waypoint.kind {
            WaypointKind::Normal => "NORMAL",
            WaypointKind::Death => "DEATH",
        });
        out.push_str("\",\n      \"iconItemId\": \"");
        out.push_str(&json::escape(&waypoint.icon_item_id));
        out.push_str("\",\n      \"markerLabel\": \"");
        out.push_str(&json::escape(&waypoint.marker_label));
        out.push_str("\",\n      \"createdAtEpochMs\": ");
        out.push_str(&waypoint.created_at_ms.to_string());
        out.push_str(",\n      \"revision\": ");
        out.push_str(&waypoint.revision.to_string());
        out.push_str("\n    }");
    }
    out.push_str("\n  ]\n}\n");
    Ok(out)
}

/// Parses a document that has already been schema-checked.
fn from_document(root: &Value) -> Result<Snapshot, String> {
    let revision = root
        .get("revision")
        .and_then(Value::as_i64)
        .ok_or("revision is missing or not an integer")?;
    if revision < 0 {
        return Err("revision is negative".to_string());
    }
    let list = root.get("waypoints").and_then(Value::as_array).ok_or("waypoints is missing")?;
    if list.len() > MAX_PERSISTED_WAYPOINTS {
        return Err(format!("too many persisted waypoints: {}", list.len()));
    }
    let mut waypoints = Vec::with_capacity(list.len());
    let mut seen: Vec<Id> = Vec::with_capacity(list.len());
    for entry in list {
        let waypoint = from_entry(entry)?;
        if waypoint.revision > revision {
            return Err("a waypoint's revision is past the catalog's".to_string());
        }
        if seen.contains(&waypoint.id) {
            return Err(format!("duplicate shared waypoint id {}", waypoint.id));
        }
        seen.push(waypoint.id);
        waypoints.push(waypoint);
    }
    if revision == 0 && !waypoints.is_empty() {
        return Err("revision zero cannot contain waypoints".to_string());
    }
    Ok(Snapshot { revision, waypoints })
}

fn from_entry(entry: &Value) -> Result<Waypoint, String> {
    let text = |field: &str| -> Result<String, String> {
        entry
            .get(field)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("{field} is missing or not a string"))
    };
    let number = |field: &str| -> Result<f64, String> {
        entry
            .get(field)
            .and_then(Value::as_f64)
            .ok_or_else(|| format!("{field} is missing or not a number"))
    };

    let id = Id::parse(&text("id")?).ok_or("id is not a canonical UUID")?;
    let publisher_id = Id::parse(&text("publisherId")?)
        .ok_or("publisherId is not a canonical UUID")?;
    let publisher_name = text("publisherName")?;
    let name = text("name")?;
    let dimension_id = text("dimensionId")?;
    let (x, y, z) = (number("x")?, number("y")?, number("z")?);
    let color_argb = entry
        .get("colorArgb")
        .and_then(Value::as_i64)
        .ok_or("colorArgb is missing or not an integer")? as i32;
    let kind = match text("type")?.as_str() {
        "NORMAL" => WaypointKind::Normal,
        "DEATH" => WaypointKind::Death,
        other => return Err(format!("unknown waypoint type {other}")),
    };
    let icon_item_id = entry
        .get("iconItemId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let marker_label = entry
        .get("markerLabel")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let created_at_ms = entry
        .get("createdAtEpochMs")
        .and_then(Value::as_i64)
        .ok_or("createdAtEpochMs is missing or not an integer")?;
    let revision = entry
        .get("revision")
        .and_then(Value::as_i64)
        .ok_or("revision is missing or not an integer")?;

    if created_at_ms < 0 || revision < 1 {
        return Err("a waypoint has a negative timestamp or a revision below one".to_string());
    }
    if !model::valid_publisher_name(&publisher_name) {
        return Err("a waypoint's publisher name is not displayable".to_string());
    }

    let draft = model::Draft {
        name,
        dimension_id,
        x,
        y,
        z,
        color: color_argb,
        kind,
        icon_item_id,
        marker_label,
    };
    let validated = model::validate(&draft)
        .ok_or("a waypoint is not valid for any world this server could load")?;

    Ok(Waypoint {
        id,
        publisher_id,
        publisher_name: publisher_name.trim().to_string(),
        name: validated.name,
        dimension_id: validated.dimension_id,
        x: validated.x,
        y: validated.y,
        z: validated.z,
        color_argb: validated.color,
        kind: validated.kind,
        icon_item_id: validated.icon_item_id,
        marker_label: validated.marker_label,
        created_at_ms,
        revision,
    })
}

/// Whether a stored waypoint may be written back out.
fn persistable(waypoint: &Waypoint, global_revision: i64) -> bool {
    let draft = model::Draft {
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
    model::validate(&draft).is_some()
        && model::valid_publisher_name(&waypoint.publisher_name)
        && waypoint.created_at_ms >= 0
        && waypoint.revision >= 1
        && waypoint.revision <= global_revision
        && waypoint.x.abs() <= MAX_HORIZONTAL_COORDINATE
        && waypoint.z.abs() <= MAX_HORIZONTAL_COORDINATE
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: &str = "00000000-0000-0000-0000-0000000000aa";

    fn waypoint(id: u64, revision: i64) -> Waypoint {
        Waypoint {
            id: Id { high: 0, low: id },
            publisher_id: Id {
                high: 0,
                low: 0xaaaa,
            },
            publisher_name: "Steve".to_string(),
            name: format!("point {id}"),
            dimension_id: "minecraft:overworld".to_string(),
            x: 12.5,
            y: 64.0,
            z: -7.25,
            color_argb: 0xff34_98dbu32 as i32,
            kind: WaypointKind::Normal,
            icon_item_id: "minecraft:compass".to_string(),
            marker_label: "B".to_string(),
            created_at_ms: 1_789_288_297_874,
            revision,
        }
    }

    fn scratch(name: &str) -> String {
        let folder = std::env::temp_dir().join(format!(
            "cfm-persist-{name}-{}-{:?}",
            std::process::id(),
            crate::identity::random_id()
        ));
        fs::create_dir_all(&folder).expect("temp dir");
        folder.to_string_lossy().to_string()
    }

    fn ready(loaded: Loaded) -> (Snapshot, Vec<String>) {
        match loaded {
            Loaded::Ready { snapshot, warnings } => (snapshot, warnings),
            Loaded::UnsupportedSchema { version } => panic!("unexpected schema {version}"),
        }
    }

    #[test]
    fn a_missing_file_is_an_empty_catalog() {
        let folder = scratch("missing");
        let persistence = Persistence::new(&folder, Some(OWNER));
        let (snapshot, warnings) = ready(persistence.load());
        assert_eq!(snapshot, Snapshot::empty());
        assert!(warnings.is_empty());
        fs::remove_dir_all(&folder).expect("clean up");
    }

    #[test]
    fn a_catalog_round_trips_through_the_file() {
        let folder = scratch("roundtrip");
        let persistence = Persistence::new(&folder, Some(OWNER));
        let snapshot = Snapshot {
            revision: 4,
            waypoints: vec![waypoint(1, 3), waypoint(2, 4)],
        };
        persistence.save(&snapshot).expect("writes");

        let (loaded, warnings) = ready(persistence.load());
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(loaded, snapshot, "every field survives the round trip");
        // The document is the reference shape, so a file from a Paper server
        // loads here and vice versa.
        let text = fs::read_to_string(persistence.path()).expect("readable");
        for key in [
            "schemaVersion",
            "revision",
            "ownerInstanceId",
            "waypoints",
            "publisherId",
            "createdAtEpochMs",
            "markerLabel",
        ] {
            assert!(text.contains(key), "{key} missing from {text}");
        }
        assert!(text.contains("\"type\": \"NORMAL\""));
        fs::remove_dir_all(&folder).expect("clean up");
    }

    #[test]
    fn an_older_schema_is_migrated_forward() {
        let folder = scratch("migrate");
        let persistence = Persistence::new(&folder, Some(OWNER));
        let document = r#"{
          "schemaVersion": 1,
          "revision": 1,
          "ownerInstanceId": "00000000-0000-0000-0000-0000000000aa",
          "waypoints": [{
            "id": "00000000-0000-0000-0000-000000000001",
            "publisherId": "00000000-0000-0000-0000-0000000000aa",
            "publisherName": "Steve", "name": "Base",
            "dimensionId": "minecraft:overworld",
            "x": 1.0, "y": 64.0, "z": 2.0, "colorArgb": -16711936,
            "type": "NORMAL", "createdAtEpochMs": 5, "revision": 1
          }]
        }"#;
        fs::write(persistence.path(), document).expect("seed an old file");

        let (snapshot, warnings) = ready(persistence.load());
        assert_eq!(snapshot.revision, 1);
        assert_eq!(snapshot.waypoints.len(), 1);
        assert_eq!(snapshot.waypoints[0].icon_item_id, "", "a pre-minor-3 field");
        assert!(warnings.iter().any(|w| w.contains("migrating")), "{warnings:?}");
        // The migrated file is now the current schema.
        let text = fs::read_to_string(persistence.path()).expect("readable");
        assert!(text.contains("\"schemaVersion\": 2"));
        fs::remove_dir_all(&folder).expect("clean up");
    }

    #[test]
    fn a_corrupt_file_is_quarantined_rather_than_served() {
        for (name, content) in [
            ("json", "{not json"),
            ("shape", r#"{"schemaVersion":2,"revision":1,"waypoints":{}}"#),
            ("revision", r#"{"schemaVersion":2,"revision":-1,"waypoints":[]}"#),
            (
                "value",
                r#"{"schemaVersion":2,"revision":1,"waypoints":[{
                    "id":"00000000-0000-0000-0000-000000000001",
                    "publisherId":"00000000-0000-0000-0000-0000000000aa",
                    "publisherName":"Steve","name":"Base",
                    "dimensionId":"minecraft:overworld","x":1.0,"y":64.0,"z":2.0,
                    "colorArgb":0,"type":"NORMAL","createdAtEpochMs":5,"revision":1}]}"#,
            ),
        ] {
            let folder = scratch(&format!("corrupt-{name}"));
            let persistence = Persistence::new(&folder, Some(OWNER));
            fs::write(persistence.path(), content).expect("seed a bad file");

            let (snapshot, warnings) = ready(persistence.load());
            assert_eq!(snapshot, Snapshot::empty(), "{name} must not be served");
            assert!(
                warnings.iter().any(|w| w.contains("unusable")),
                "{name}: {warnings:?}"
            );
            assert!(
                std::path::Path::new(&format!("{}.bad", persistence.path())).exists(),
                "{name}: the file must be kept for inspection"
            );
            fs::remove_dir_all(&folder).expect("clean up");
        }
    }

    #[test]
    fn a_newer_schema_is_left_alone() {
        let folder = scratch("future");
        let persistence = Persistence::new(&folder, Some(OWNER));
        let document = r#"{"schemaVersion": 99, "revision": 0, "waypoints": []}"#;
        fs::write(persistence.path(), document).expect("seed");

        assert_eq!(
            persistence.load(),
            Loaded::UnsupportedSchema { version: 99 }
        );
        assert_eq!(
            fs::read_to_string(persistence.path()).expect("readable"),
            document,
            "a file this build cannot read must not be touched or quarantined"
        );
        fs::remove_dir_all(&folder).expect("clean up");
    }

    #[test]
    fn a_file_from_another_server_instance_is_set_aside() {
        let folder = scratch("inherited");
        let writer = Persistence::new(&folder, Some("11111111-1111-1111-1111-111111111111"));
        writer
            .save(&Snapshot {
                revision: 1,
                waypoints: vec![waypoint(1, 1)],
            })
            .expect("writes");

        let reader = Persistence::new(&folder, Some(OWNER));
        let (snapshot, warnings) = ready(reader.load());
        assert_eq!(
            snapshot,
            Snapshot::empty(),
            "another server's catalog is not this server's"
        );
        assert!(
            warnings.iter().any(|w| w.contains("belong to server instance")),
            "{warnings:?}"
        );
        assert!(std::path::Path::new(&format!("{}.bak", reader.path())).exists());
        fs::remove_dir_all(&folder).expect("clean up");
    }

    #[test]
    fn a_file_with_no_owner_is_claimed_and_restamped() {
        let folder = scratch("ownerless");
        let persistence = Persistence::new(&folder, Some(OWNER));
        fs::write(
            persistence.path(),
            r#"{"schemaVersion":2,"revision":1,"ownerInstanceId":null,"waypoints":[{
                "id":"00000000-0000-0000-0000-000000000001",
                "publisherId":"00000000-0000-0000-0000-0000000000aa",
                "publisherName":"Steve","name":"Base",
                "dimensionId":"minecraft:overworld","x":1.0,"y":64.0,"z":2.0,
                "colorArgb":-16711936,"type":"NORMAL","createdAtEpochMs":5,"revision":1}]}"#,
        )
        .expect("seed");

        let (snapshot, warnings) = ready(persistence.load());
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(snapshot.waypoints.len(), 1, "an unowned file is adopted");
        persistence.save(&snapshot).expect("restamp");
        let text = fs::read_to_string(persistence.path()).expect("readable");
        assert!(text.contains(OWNER), "the adopted file is stamped");
        fs::remove_dir_all(&folder).expect("clean up");
    }

    #[test]
    fn text_that_needs_escaping_survives() {
        let folder = scratch("escaping");
        let persistence = Persistence::new(&folder, Some(OWNER));
        let mut point = waypoint(1, 1);
        point.name = "quote\" and \\ and 地图".to_string();
        let snapshot = Snapshot {
            revision: 1,
            waypoints: vec![point],
        };
        persistence.save(&snapshot).expect("writes");
        let (loaded, _) = ready(persistence.load());
        assert_eq!(loaded.waypoints[0].name, "quote\" and \\ and 地图");
        fs::remove_dir_all(&folder).expect("clean up");
    }
}
