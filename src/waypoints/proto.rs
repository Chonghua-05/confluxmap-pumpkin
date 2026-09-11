//! Wire codec for the `confluxmap:waypoints_v1` channel.
//!
//! A byte-for-byte mirror of the reference implementation in
//! `common/src/main/java/cn/net/rms/confluxmap/core/net/shared/`. Every message
//! is one type byte followed by a fixed body; multi-byte values are big-endian
//! (see [`crate::wire`]); strings are `u16`-length-prefixed UTF-8.
//!
//! # Directions
//!
//! Six client-to-server messages and five server-to-client ones live in one
//! type-id space, and the reference codec refuses a message that arrives in the
//! wrong direction before decoding its body. [`decode`] does the same: a
//! `STATUS` arriving from a client is a malformed payload, not an unknown one.
//!
//! # Protocol minor
//!
//! The channel's own version is negotiated in the first exchange: the client's
//! `HELLO` carries its minor, and every later frame on the connection is written
//! in the lower of the two versions. Two fields are gated this way:
//!
//! * minor >= 2 adds `ownerManagementAllowed` to `STATUS`;
//! * minor >= 3 adds `iconItemId` and `markerLabel` to every waypoint.
//!
//! A pre-minor field is not merely omitted - it is not read either, which is why
//! the negotiated minor has to be threaded through both directions.

use std::fmt;

use crate::identity::Id;
use crate::wire::{self, Reader};

/// Registered channel (`SharedWaypointProto.CHANNEL_ID`).
pub const CHANNEL_ID: &str = "confluxmap:waypoints_v1";
/// Protocol major this build speaks.
pub const PROTO_MAJOR: i32 = 1;
/// Protocol minor this build speaks: the version that introduced marker styles.
pub const PROTO_MINOR: i32 = 3;

/// `0x01 C2S HELLO`.
pub const MSG_HELLO: u8 = 0x01;
/// `0x02 S2C STATUS`.
pub const MSG_STATUS: u8 = 0x02;
/// `0x03 C2S SUBSCRIBE`.
pub const MSG_SUBSCRIBE: u8 = 0x03;
/// `0x04 C2S CREATE`.
pub const MSG_CREATE: u8 = 0x04;
/// `0x05 C2S DELETE`.
pub const MSG_DELETE: u8 = 0x05;
/// `0x06 C2S LOCK`.
pub const MSG_LOCK: u8 = 0x06;
/// `0x07 S2C SNAPSHOT`.
pub const MSG_SNAPSHOT: u8 = 0x07;
/// `0x08 S2C UPSERT`.
pub const MSG_UPSERT: u8 = 0x08;
/// `0x09 S2C REMOVE`.
pub const MSG_REMOVE: u8 = 0x09;
/// `0x0A S2C RESULT`.
pub const MSG_RESULT: u8 = 0x0A;
/// `0x0B C2S UPDATE`.
pub const MSG_UPDATE: u8 = 0x0B;

/// Cap on a client-to-server payload.
pub const MAX_C2S_PAYLOAD: usize = 8 * 1024;
/// Cap on any UTF-8 field.
pub const MAX_UTF8_BYTES: usize = 256;
/// Cap on the number of waypoints in one snapshot.
pub const MAX_SNAPSHOT_WAYPOINTS: usize = 512;

/// `RESULT_STATUS_APPLIED`.
pub const RESULT_STATUS_APPLIED: i32 = 0;
/// `RESULT_STATUS_REJECTED`.
pub const RESULT_STATUS_REJECTED: i32 = 1;

/// `RESULT_ERROR_NONE`.
pub const RESULT_ERROR_NONE: i32 = 0;
/// `RESULT_ERROR_INVALID_REQUEST`.
pub const RESULT_ERROR_INVALID_REQUEST: i32 = 1;
/// `RESULT_ERROR_REVISION_CONFLICT`.
pub const RESULT_ERROR_REVISION_CONFLICT: i32 = 2;
/// `RESULT_ERROR_NOT_FOUND`.
pub const RESULT_ERROR_NOT_FOUND: i32 = 3;
/// `RESULT_ERROR_FORBIDDEN`.
pub const RESULT_ERROR_FORBIDDEN: i32 = 4;
/// `RESULT_ERROR_WORLD_QUOTA_EXCEEDED`.
pub const RESULT_ERROR_WORLD_QUOTA_EXCEEDED: i32 = 5;
/// `RESULT_ERROR_PLAYER_QUOTA_EXCEEDED`.
pub const RESULT_ERROR_PLAYER_QUOTA_EXCEEDED: i32 = 6;
/// `RESULT_ERROR_RATE_LIMITED`.
pub const RESULT_ERROR_RATE_LIMITED: i32 = 7;
/// `RESULT_ERROR_PERSISTENCE_FAILED`.
pub const RESULT_ERROR_PERSISTENCE_FAILED: i32 = 8;
/// `RESULT_ERROR_ID_GENERATION_FAILED`.
pub const RESULT_ERROR_ID_GENERATION_FAILED: i32 = 9;
/// `RESULT_ERROR_DISABLED`.
pub const RESULT_ERROR_DISABLED: i32 = 10;
/// `RESULT_ERROR_DUPLICATE_LOCATION`.
pub const RESULT_ERROR_DUPLICATE_LOCATION: i32 = 11;

/// The waypoint kind byte in a `Waypoint.Type` field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaypointKind {
    /// `Waypoint.Type.NORMAL`
    Normal,
    /// `Waypoint.Type.DEATH`
    Death,
}

impl WaypointKind {
    /// The wire value.
    fn wire(self) -> u8 {
        match self {
            WaypointKind::Normal => 0,
            WaypointKind::Death => 1,
        }
    }

    fn from_wire(value: u8) -> Option<Self> {
        match value {
            0 => Some(WaypointKind::Normal),
            1 => Some(WaypointKind::Death),
            _ => None,
        }
    }
}

/// One server-owned waypoint (`core/shared/SharedWaypoint`).
///
/// Identities are [`Id`] rather than the plugin API's `Uuid`, which is a WIT
/// record with no `PartialEq` and so cannot be compared or keyed - and this
/// protocol compares them constantly, from correlating a `RESULT` back to its
/// request to looking a point up by id. The wire form is unchanged: [`Id`] is
/// the same two big-endian halves.
///
/// Coordinates are in the local coordinate system of `dimension_id`.
/// `revision` is the global revision the point was created or last changed at,
/// which is what the client's optimistic-update check compares against.
#[derive(Clone, Debug, PartialEq)]
pub struct Waypoint {
    /// Server-assigned id.
    pub id: Id,
    /// The player who published it.
    pub publisher_id: Id,
    /// Their name at publish time, kept for display.
    pub publisher_name: String,
    /// Display name.
    pub name: String,
    /// Dimension resource id, e.g. `minecraft:overworld`.
    pub dimension_id: String,
    /// Block coordinates.
    pub x: f64,
    /// See [`Waypoint::x`].
    pub y: f64,
    /// See [`Waypoint::x`].
    pub z: f64,
    /// Opaque ARGB colour; the client requires an opaque alpha.
    pub color_argb: i32,
    /// Normal or death marker.
    pub kind: WaypointKind,
    /// Marker icon item, or empty (protocol minor >= 3).
    pub icon_item_id: String,
    /// Marker label of at most three code points, or empty (minor >= 3).
    pub marker_label: String,
    /// Creation time, milliseconds since the Unix epoch.
    pub created_at_ms: i64,
    /// Global revision this point was written at.
    pub revision: i64,
}

/// A create request, as it arrives.
#[derive(Clone, Debug, PartialEq)]
pub struct CreateRequest {
    /// Client-generated correlation id.
    pub operation_id: Id,
    /// The global revision the client believed it was writing against.
    pub expected_revision: i64,
    /// Display name.
    pub name: String,
    /// Dimension resource id.
    pub dimension_id: String,
    /// Block coordinates.
    pub x: f64,
    /// See [`CreateRequest::x`].
    pub y: f64,
    /// See [`CreateRequest::x`].
    pub z: f64,
    /// Opaque ARGB colour.
    pub color: i32,
    /// Normal or death marker.
    pub kind: WaypointKind,
    /// Marker icon item, or empty.
    pub icon_item_id: String,
    /// Marker label, or empty.
    pub marker_label: String,
}

/// An update request, as it arrives.
#[derive(Clone, Debug, PartialEq)]
pub struct UpdateRequest {
    /// Client-generated correlation id.
    pub operation_id: Id,
    /// The waypoint to replace.
    pub id: Id,
    /// The revision the client believed the point was at.
    pub expected_revision: i64,
    /// Display name.
    pub name: String,
    /// Dimension resource id.
    pub dimension_id: String,
    /// Block coordinates.
    pub x: f64,
    /// See [`UpdateRequest::x`].
    pub y: f64,
    /// See [`UpdateRequest::x`].
    pub z: f64,
    /// Opaque ARGB colour.
    pub color: i32,
    /// Normal or death marker.
    pub kind: WaypointKind,
    /// Marker icon item, or empty.
    pub icon_item_id: String,
    /// Marker label, or empty.
    pub marker_label: String,
}

/// A delete request, as it arrives.
#[derive(Clone, Debug, PartialEq)]
pub struct DeleteRequest {
    /// Client-generated correlation id.
    pub operation_id: Id,
    /// The waypoint to remove.
    pub id: Id,
    /// The revision the client believed the point was at.
    pub expected_revision: i64,
}

/// A lock request, as it arrives.
///
/// The reference server answers every one of these with
/// `INVALID_REQUEST`: server markers no longer exist, and the field that used to
/// carry the flag is kept on the wire only so older clients can still be
/// answered. This plugin decodes it in order to answer it the same way.
#[derive(Clone, Debug, PartialEq)]
pub struct LockRequest {
    /// Client-generated correlation id.
    pub operation_id: Id,
    /// The waypoint the client meant.
    pub id: Id,
    /// The revision the client believed the point was at.
    pub expected_revision: i64,
    /// The requested lock state.
    pub locked: bool,
}

/// One decoded client-to-server message.
#[derive(Clone, Debug, PartialEq)]
pub enum Inbound {
    /// `HELLO`: the client announces its protocol version.
    Hello {
        /// Its major.
        major: i32,
        /// Its minor.
        minor: i32,
    },
    /// `SUBSCRIBE`: send the snapshot and stream changes.
    Subscribe,
    /// `CREATE`.
    Create(CreateRequest),
    /// `UPDATE`.
    Update(UpdateRequest),
    /// `DELETE`.
    Delete(DeleteRequest),
    /// `LOCK`.
    Lock(LockRequest),
}

/// One server-to-client message.
#[derive(Clone, Debug, PartialEq)]
pub enum Outbound {
    /// `STATUS`: capability and world status, answered to every `HELLO`.
    Status {
        /// Protocol major this server speaks.
        major: i32,
        /// Negotiated minor.
        minor: i32,
        /// Whether this server speaks the channel at all.
        supported: bool,
        /// Whether the feature is switched on right now.
        enabled: bool,
        /// Whether the peer is an operator.
        operator: bool,
        /// Namespace the client files this server's points under.
        world_id: String,
        /// Current global revision.
        revision: i64,
        /// Catalog size limit.
        max_world: i32,
        /// Per-player publish limit.
        max_player: i32,
        /// Whether non-operators may manage their own points.
        owner_management_allowed: bool,
    },
    /// `SNAPSHOT`: the full catalog at one revision.
    Snapshot {
        /// Revision the list is at.
        revision: i64,
        /// Whether the peer is an operator.
        operator: bool,
        /// Every point.
        waypoints: Vec<Waypoint>,
    },
    /// `UPSERT`: one added or changed point.
    Upsert {
        /// The new global revision.
        revision: i64,
        /// The point.
        waypoint: Waypoint,
    },
    /// `REMOVE`: one deleted point.
    Remove {
        /// The new global revision.
        revision: i64,
        /// The point that is gone.
        id: Id,
    },
    /// `RESULT`: the outcome of one mutation, correlated by operation id.
    Result {
        /// The client's correlation id.
        operation_id: Id,
        /// `RESULT_STATUS_*`.
        status: i32,
        /// `RESULT_ERROR_*`.
        error: i32,
    },
}

impl Outbound {
    /// The message's name, for log lines.
    pub fn kind(&self) -> &'static str {
        match self {
            Outbound::Status { .. } => "STATUS",
            Outbound::Snapshot { .. } => "SNAPSHOT",
            Outbound::Upsert { .. } => "UPSERT",
            Outbound::Remove { .. } => "REMOVE",
            Outbound::Result { .. } => "RESULT",
        }
    }
}

/// A payload that violated the wire contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodeError(pub String);/// A message that cannot be represented in the negotiated wire shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodeError(pub String);

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

fn decode_error(message: impl Into<String>) -> DecodeError {
    DecodeError(message.into())
}

/// Decodes exactly one client-to-server message.
///
/// `negotiated_minor` is the connection's agreed minor, which decides whether
/// the trailing marker-style fields are present. Before a `HELLO` establishes
/// one, the reference client and server both use `0`, so that is the value to
/// pass.
pub fn decode(payload: &[u8], negotiated_minor: i32) -> Result<Inbound, DecodeError> {
    if payload.is_empty() {
        return Err(decode_error("empty payload"));
    }
    if payload.len() > MAX_C2S_PAYLOAD {
        return Err(decode_error(format!(
            "C2S payload of {} bytes exceeds cap {MAX_C2S_PAYLOAD}",
            payload.len()
        )));
    }
    let type_id = payload[0];
    if !is_known(type_id) {
        return Err(decode_error(format!(
            "unknown message type 0x{type_id:02x}"
        )));
    }
    if is_serverbound(type_id) {
        return Err(decode_error(format!(
            "message type 0x{type_id:02x} is not valid in the C2S direction"
        )));
    }

    let mut r = Reader::new(&payload[1..]);
    let message = match type_id {
        MSG_HELLO => Inbound::Hello {
            major: r.i32().ok_or_else(|| decode_error("truncated major"))?,
            minor: r.i32().ok_or_else(|| decode_error("truncated minor"))?,
        },
        MSG_SUBSCRIBE => Inbound::Subscribe,
        MSG_CREATE => Inbound::Create(CreateRequest {
            operation_id: r.uuid().ok_or_else(|| decode_error("truncated operationId"))?,
            expected_revision: r.i64().ok_or_else(|| decode_error("truncated revision"))?,
            name: r.utf(MAX_UTF8_BYTES).ok_or_else(|| decode_error("invalid name"))?,
            dimension_id: r
                .utf(MAX_UTF8_BYTES)
                .ok_or_else(|| decode_error("invalid dimensionId"))?,
            x: finite(&mut r, "x")?,
            y: finite(&mut r, "y")?,
            z: finite(&mut r, "z")?,
            color: r.i32().ok_or_else(|| decode_error("truncated colour"))?,
            kind: kind(&mut r)?,
            icon_item_id: marker_utf(&mut r, negotiated_minor, "iconItemId")?,
            marker_label: marker_utf(&mut r, negotiated_minor, "markerLabel")?,
        }),
        MSG_UPDATE => Inbound::Update(UpdateRequest {
            operation_id: r.uuid().ok_or_else(|| decode_error("truncated operationId"))?,
            id: r.uuid().ok_or_else(|| decode_error("truncated id"))?,
            expected_revision: r.i64().ok_or_else(|| decode_error("truncated revision"))?,
            name: r.utf(MAX_UTF8_BYTES).ok_or_else(|| decode_error("invalid name"))?,
            dimension_id: r
                .utf(MAX_UTF8_BYTES)
                .ok_or_else(|| decode_error("invalid dimensionId"))?,
            x: finite(&mut r, "x")?,
            y: finite(&mut r, "y")?,
            z: finite(&mut r, "z")?,
            color: r.i32().ok_or_else(|| decode_error("truncated colour"))?,
            kind: kind(&mut r)?,
            icon_item_id: marker_utf(&mut r, negotiated_minor, "iconItemId")?,
            marker_label: marker_utf(&mut r, negotiated_minor, "markerLabel")?,
        }),
        MSG_DELETE => Inbound::Delete(DeleteRequest {
            operation_id: r.uuid().ok_or_else(|| decode_error("truncated operationId"))?,
            id: r.uuid().ok_or_else(|| decode_error("truncated id"))?,
            expected_revision: r.i64().ok_or_else(|| decode_error("truncated revision"))?,
        }),
        MSG_LOCK => Inbound::Lock(LockRequest {
            operation_id: r.uuid().ok_or_else(|| decode_error("truncated operationId"))?,
            id: r.uuid().ok_or_else(|| decode_error("truncated id"))?,
            expected_revision: r.i64().ok_or_else(|| decode_error("truncated revision"))?,
            locked: r
                .bool()
                .ok_or_else(|| decode_error("locked must be encoded as 0 or 1"))?,
        }),
        other => return Err(decode_error(format!("unhandled message type 0x{other:02x}"))),
    };

    if r.remaining() != 0 {
        return Err(decode_error(format!(
            "trailing bytes after message: {}",
            r.remaining()
        )));
    }
    Ok(message)
}

/// Encodes one server-to-client message.
///
/// Encoding is total for every value the service can produce, so an error here
/// means a bug rather than untrusted input - but it still returns a `Result`
/// instead of panicking inside a host callback.
pub fn encode(message: &Outbound, negotiated_minor: i32) -> Result<Vec<u8>, EncodeError> {
    let mut out = Vec::with_capacity(64);
    match message {
        Outbound::Status {
            major,
            minor: _,
            supported,
            enabled,
            operator,
            world_id,
            revision,
            max_world,
            max_player,
            owner_management_allowed,
        } => {
            // The frame reports the negotiated minor, not this build's minor:
            // the client reads its own gate out of this field.
            let minor = negotiated_minor.max(0);
            out.push(MSG_STATUS);
            wire::i32(&mut out, *major);
            wire::i32(&mut out, minor);
            wire::bool(&mut out, *supported);
            wire::bool(&mut out, *enabled);
            wire::bool(&mut out, *operator);
            put_utf(&mut out, world_id, "worldId")?;
            wire::i64(&mut out, *revision);
            wire::i32(&mut out, *max_world);
            wire::i32(&mut out, *max_player);
            if minor >= 2 {
                wire::bool(&mut out, *owner_management_allowed);
            }
        }
        Outbound::Snapshot {
            revision,
            operator,
            waypoints,
        } => {
            if waypoints.len() > MAX_SNAPSHOT_WAYPOINTS {
                return Err(EncodeError(format!(
                    "snapshot of {} waypoints exceeds cap {MAX_SNAPSHOT_WAYPOINTS}",
                    waypoints.len()
                )));
            }
            out.push(MSG_SNAPSHOT);
            wire::i64(&mut out, *revision);
            wire::bool(&mut out, *operator);
            out.extend_from_slice(&(waypoints.len() as u16).to_be_bytes());
            for waypoint in waypoints {
                put_waypoint(&mut out, waypoint, negotiated_minor)?;
            }
        }
        Outbound::Upsert { revision, waypoint } => {
            out.push(MSG_UPSERT);
            wire::i64(&mut out, *revision);
            put_waypoint(&mut out, waypoint, negotiated_minor)?;
        }
        Outbound::Remove { revision, id } => {
            out.push(MSG_REMOVE);
            wire::i64(&mut out, *revision);
            wire::uuid(&mut out, *id);
        }
        Outbound::Result {
            operation_id,
            status,
            error,
        } => {
            out.push(MSG_RESULT);
            wire::uuid(&mut out, *operation_id);
            wire::i32(&mut out, *status);
            wire::i32(&mut out, *error);
        }
    }
    Ok(out)
}

/// Appends one waypoint entry, including the minor-gated marker style.
fn put_waypoint(
    out: &mut Vec<u8>,
    waypoint: &Waypoint,
    negotiated_minor: i32,
) -> Result<(), EncodeError> {
    wire::uuid(out, waypoint.id);
    wire::uuid(out, waypoint.publisher_id);
    put_utf(out, &waypoint.publisher_name, "publisherName")?;
    put_utf(out, &waypoint.name, "name")?;
    put_utf(out, &waypoint.dimension_id, "dimensionId")?;
    for (value, field) in [
        (waypoint.x, "x"),
        (waypoint.y, "y"),
        (waypoint.z, "z"),
    ] {
        if !value.is_finite() {
            return Err(EncodeError(format!("{field} coordinate must be finite")));
        }
        wire::f64(out, value);
    }
    wire::i32(out, waypoint.color_argb);
    wire::u8(out, waypoint.kind.wire());
    // The v1 shape carried a per-point operator lock. Server markers were
    // removed, but the byte stays so older clients still parse the entry.
    wire::bool(out, false);
    wire::i64(out, waypoint.created_at_ms);
    wire::i64(out, waypoint.revision);
    if negotiated_minor >= 3 {
        put_utf(out, &waypoint.icon_item_id, "iconItemId")?;
        put_utf(out, &waypoint.marker_label, "markerLabel")?;
    }
    Ok(())
}

fn put_utf(out: &mut Vec<u8>, value: &str, field: &str) -> Result<(), EncodeError> {
    wire::utf(out, value, MAX_UTF8_BYTES)
        .map_err(|long| EncodeError(format!("{field} is {} bytes, over {MAX_UTF8_BYTES}", long.len)))
}

fn finite(r: &mut Reader<'_>, field: &str) -> Result<f64, DecodeError> {
    let value = r
        .f64()
        .ok_or_else(|| decode_error(format!("truncated {field}")))?;
    if !value.is_finite() {
        return Err(decode_error(format!("{field} coordinate must be finite")));
    }
    Ok(value)
}

fn kind(r: &mut Reader<'_>) -> Result<WaypointKind, DecodeError> {
    let value = r.u8().ok_or_else(|| decode_error("truncated waypoint type"))?;
    WaypointKind::from_wire(value)
        .ok_or_else(|| decode_error(format!("unknown waypoint type id: {value}")))
}

/// Reads a marker-style field only when the connection negotiated it.
fn marker_utf(
    r: &mut Reader<'_>,
    negotiated_minor: i32,
    field: &str,
) -> Result<String, DecodeError> {
    if negotiated_minor < 3 {
        return Ok(String::new());
    }
    r.utf(MAX_UTF8_BYTES)
        .ok_or_else(|| decode_error(format!("invalid {field}")))
}

fn is_known(type_id: u8) -> bool {
    (MSG_HELLO..=MSG_UPDATE).contains(&type_id)
}

fn is_serverbound(type_id: u8) -> bool {
    matches!(
        type_id,
        MSG_STATUS | MSG_SNAPSHOT | MSG_UPSERT | MSG_REMOVE | MSG_RESULT
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid(high: u64, low: u64) -> Id {
        Id { high, low }
    }

    fn sample_waypoint() -> Waypoint {
        Waypoint {
            id: uuid(0x0011_2233_4455_6677, 0x8899_aabb_ccdd_eeff),
            publisher_id: uuid(0x0102_0304_0506_0708, 0x090a_0b0c_0d0e_0f10),
            publisher_name: "Steve".to_string(),
            name: "Base".to_string(),
            dimension_id: "minecraft:overworld".to_string(),
            x: 12.5,
            y: 64.0,
            z: -7.25,
            color_argb: 0xff34_98dbu32 as i32,
            kind: WaypointKind::Normal,
            icon_item_id: "minecraft:compass".to_string(),
            marker_label: "B".to_string(),
            created_at_ms: 1_789_288_297_874,
            revision: 3,
        }
    }

    fn hex(bytes: &[u8]) -> String {
        crate::state::hex(bytes)
    }

    #[test]
    fn hello_is_two_big_endian_ints() {
        let payload = [MSG_HELLO, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x03];
        assert_eq!(
            decode(&payload, 3).expect("valid hello"),
            Inbound::Hello { major: 1, minor: 3 }
        );
    }

    #[test]
    fn subscribe_is_a_bare_type_byte() {
        assert_eq!(decode(&[MSG_SUBSCRIBE], 3).expect("valid"), Inbound::Subscribe);
        // ... and trailing bytes are a desynchronised peer, not a payload to guess at.
        assert!(decode(&[MSG_SUBSCRIBE, 0x00], 3).is_err());
    }

    #[test]
    fn a_capability_less_connection_has_no_marker_style() {
        let mut payload = vec![MSG_CREATE];
        wire::uuid(&mut payload, uuid(1, 2));
        wire::i64(&mut payload, 4);
        wire::utf(&mut payload, "Base", MAX_UTF8_BYTES).expect("fits");
        wire::utf(&mut payload, "minecraft:overworld", MAX_UTF8_BYTES).expect("fits");
        wire::f64(&mut payload, 1.0);
        wire::f64(&mut payload, 2.0);
        wire::f64(&mut payload, 3.0);
        wire::i32(&mut payload, -1);
        wire::u8(&mut payload, 1);

        let decoded = decode(&payload, 2).expect("minor 2 has no marker style");
        let Inbound::Create(create) = decoded else {
            panic!("expected a create");
        };
        assert_eq!(create.icon_item_id, "");
        assert_eq!(create.marker_label, "");
        assert_eq!(create.kind, WaypointKind::Death);
        assert_eq!(create.expected_revision, 4);
        // At minor 3 the same bytes are short by two fields.
        assert!(decode(&payload, 3).is_err());
    }

    #[test]
    fn create_carries_the_marker_style_at_minor_three() {
        let mut payload = vec![MSG_CREATE];
        wire::uuid(&mut payload, uuid(1, 2));
        wire::i64(&mut payload, 0);
        wire::utf(&mut payload, "Base", MAX_UTF8_BYTES).expect("fits");
        wire::utf(&mut payload, "minecraft:overworld", MAX_UTF8_BYTES).expect("fits");
        wire::f64(&mut payload, 12.5);
        wire::f64(&mut payload, 64.0);
        wire::f64(&mut payload, -7.25);
        wire::i32(&mut payload, 0xff34_98dbu32 as i32);
        wire::u8(&mut payload, 0);
        wire::utf(&mut payload, "minecraft:compass", MAX_UTF8_BYTES).expect("fits");
        wire::utf(&mut payload, "B", MAX_UTF8_BYTES).expect("fits");

        let Inbound::Create(create) = decode(&payload, 3).expect("valid") else {
            panic!("expected a create");
        };
        assert_eq!(create.name, "Base");
        assert_eq!(create.icon_item_id, "minecraft:compass");
        assert_eq!(create.marker_label, "B");
        assert_eq!(create.color, 0xff34_98dbu32 as i32);
    }

    #[test]
    fn rejects_messages_that_belong_to_the_other_direction() {
        let error = decode(&[MSG_STATUS], 3).expect_err("serverbound decode of S2C");
        assert!(error.0.contains("not valid in the C2S direction"), "{error}");
        // Unknown ids, oversized payloads and empty payloads are all malformed.
        assert!(decode(&[0x7f], 3).is_err());
        assert!(decode(&[], 3).is_err());
        assert!(decode(&vec![MSG_SUBSCRIBE; MAX_C2S_PAYLOAD + 1], 3).is_err());
    }

    #[test]
    fn rejects_non_finite_coordinates_and_unknown_types() {
        assert!(decode(&create_payload(f64::NAN, 0), 2).is_err(), "NaN is not a coordinate");
        assert!(decode(&create_payload(f64::INFINITY, 0), 2).is_err());
        assert!(decode(&create_payload(1.0, 9), 2).is_err(), "unknown waypoint type");
        assert!(decode(&create_payload(1.0, 0), 2).is_ok());
    }

    /// A well-formed minor-2 `CREATE` with a chosen `x` and kind byte.
    fn create_payload(x: f64, kind: u8) -> Vec<u8> {
        let mut payload = vec![MSG_CREATE];
        wire::uuid(&mut payload, uuid(1, 2));
        wire::i64(&mut payload, 0);
        wire::utf(&mut payload, "Base", MAX_UTF8_BYTES).expect("fits");
        wire::utf(&mut payload, "minecraft:overworld", MAX_UTF8_BYTES).expect("fits");
        for value in [x, 0.0, 0.0] {
            wire::f64(&mut payload, value);
        }
        wire::i32(&mut payload, 0);
        wire::u8(&mut payload, kind);
        payload
    }

    /// The status frame for a negotiated minor 3 connection, byte for byte: the
    /// type, this build's major, the negotiated minor, the three capability
    /// booleans, the world id, the revision, both quotas and the owner policy.
    #[test]
    fn status_layout_matches_the_reference_encoder() {
        let message = Outbound::Status {
            major: PROTO_MAJOR,
            minor: PROTO_MINOR,
            supported: true,
            enabled: true,
            operator: false,
            world_id: "00000000-0000-0000-0000-456789abcdef".to_string(),
            revision: 7,
            max_world: 512,
            max_player: 64,
            owner_management_allowed: true,
        };
        let bytes = encode(&message, 3).expect("encodes");
        assert_eq!(
            hex(&bytes),
            "0200000001000000030101000024\
             30303030303030302d303030302d303030302d303030302d343536373839616263646566\
             0000000000000007000002000000004001"
        );
        assert_eq!(bytes.len(), 67);
    }

    /// A minor-1 peer must not receive the field it cannot read.
    #[test]
    fn status_omits_the_owner_policy_before_minor_two() {
        let message = Outbound::Status {
            major: PROTO_MAJOR,
            minor: PROTO_MINOR,
            supported: true,
            enabled: true,
            operator: true,
            world_id: "w".to_string(),
            revision: 1,
            max_world: 512,
            max_player: 64,
            owner_management_allowed: true,
        };
        let at_one = encode(&message, 1).expect("encodes");
        let at_two = encode(&message, 2).expect("encodes");
        assert_eq!(at_two.len(), at_one.len() + 1);
        // The frame reports the negotiated minor, not this build's, because the
        // client reads its own gate out of this field.
        assert_eq!(at_one[5..9], [0, 0, 0, 1]);
        assert_eq!(at_two[5..9], [0, 0, 0, 2]);
        assert_eq!(*at_one.last().expect("non-empty"), 0x40, "maxPlayer low byte");
        assert_eq!(*at_two.last().expect("non-empty"), 1, "owner policy");
    }

    #[test]
    fn a_remove_frame_is_a_type_a_revision_and_an_id() {
        let message = Outbound::Remove {
            revision: 9,
            id: uuid(1, 2),
        };
        let bytes = encode(&message, 3).expect("encodes");
        assert_eq!(bytes[0], MSG_REMOVE);
        assert_eq!(bytes.len(), 1 + 8 + 16);
    }

    #[test]
    fn a_result_frame_correlates_status_and_error() {
        let message = Outbound::Result {
            operation_id: uuid(0xdead_beef, 0xfeed_face),
            status: RESULT_STATUS_REJECTED,
            error: RESULT_ERROR_DUPLICATE_LOCATION,
        };
        let bytes = encode(&message, 3).expect("encodes");
        assert_eq!(bytes[0], MSG_RESULT);
        let mut r = Reader::new(&bytes[1..]);
        assert_eq!(r.uuid(), Some(uuid(0xdead_beef, 0xfeed_face)));
        assert_eq!(r.i32(), Some(RESULT_STATUS_REJECTED));
        assert_eq!(r.i32(), Some(RESULT_ERROR_DUPLICATE_LOCATION));
        assert_eq!(r.remaining(), 0);
    }

    /// A snapshot entry has a fixed shape, and a minor-3 peer also gets the
    /// marker style. Getting this wrong shows up on the client as a rejected
    /// payload, which is a silent failure, so pin the layout.
    #[test]
    fn a_snapshot_entry_keeps_the_legacy_lock_byte() {
        let waypoint = sample_waypoint();
        let at_three = encode(
            &Outbound::Snapshot {
                revision: 3,
                operator: false,
                waypoints: vec![waypoint.clone()],
            },
            3,
        )
        .expect("encodes");
        let at_two = encode(
            &Outbound::Snapshot {
                revision: 3,
                operator: false,
                waypoints: vec![waypoint],
            },
            2,
        )
        .expect("encodes");

        // Header: type, revision, operator, u16 count.
        assert_eq!(at_three[0], MSG_SNAPSHOT);
        assert_eq!(&at_three[1..9], 3i64.to_be_bytes());
        assert_eq!(at_three[9], 0);
        assert_eq!(&at_three[10..12], 1u16.to_be_bytes());

        let entry = 16 + 16                          // id, publisherId
            + (2 + "Steve".len())                    // publisherName
            + (2 + "Base".len())                     // name
            + (2 + "minecraft:overworld".len())      // dimensionId
            + 24 + 4 + 1                             // x, y, z, colour, kind
            + 1 + 8 + 8;                             // legacy lock, createdAt, revision
        let marker = (2 + "minecraft:compass".len()) + (2 + "B".len());
        assert_eq!(at_two.len(), 12 + entry, "a minor-2 entry stops before the marker style");
        assert_eq!(at_three.len(), 12 + entry + marker);
    }
}
