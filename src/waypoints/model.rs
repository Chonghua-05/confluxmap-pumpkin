//! The shared-waypoint domain model: a point, the block it occupies, and the
//! semantic rules every untrusted create or update passes through.
//!
//! Validation is deliberately separate from the wire codec. The codec answers
//! "is this a well-formed frame?", which is a question about bytes; this module
//! answers "is this a waypoint a server should keep?", which is a question about
//! the world. Every failure collapses to one `None`, because the protocol has a
//! single `INVALID_REQUEST` error code for all of them - telling a client which
//! rule it broke would leak the server's limits for no benefit.

use crate::waypoints::proto::WaypointKind;

/// Longest waypoint name, in code points (`SharedWaypointValidator.MAX_NAME_CODE_POINTS`).
pub const MAX_NAME_CODE_POINTS: usize = 64;
/// Longest publisher name, in code points.
pub const MAX_PUBLISHER_NAME_CODE_POINTS: usize = 64;
/// Longest marker label, in code points (`WaypointMarkerStyle.MAX_LABEL_CODE_POINTS`).
pub const MAX_LABEL_CODE_POINTS: usize = 3;
/// Largest horizontal coordinate, matching the reference validator.
pub const MAX_HORIZONTAL_COORDINATE: f64 = 29_999_984.0;
/// Largest coordinate on any axis, matching the client's own check.
pub const MAX_ABSOLUTE_COORDINATE: f64 = 30_000_000.0;

/// The block a waypoint sits in.
///
/// Occupancy is part of the contract: two points at the same block, in the same
/// dimension, cannot both exist, and the client mirrors that rule so it can grey
/// out a "publish" button before sending anything. Coordinates are floored into
/// blocks, so a point at `12.9` and one at `12.1` collide.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LocationKey {
    /// Dimension resource id.
    pub dimension_id: String,
    /// Floored block coordinates.
    pub block_x: i64,
    /// See [`LocationKey::block_x`].
    pub block_y: i64,
    /// See [`LocationKey::block_x`].
    pub block_z: i64,
}

impl LocationKey {
    /// Floors a position into a block key.
    ///
    /// `None` for a non-finite value, or one outside the `i64` range a floor
    /// could represent: both are things a hostile client can send, and neither
    /// should reach the occupancy index.
    pub fn of(dimension_id: &str, x: f64, y: f64, z: f64) -> Option<Self> {
        Some(LocationKey {
            dimension_id: dimension_id.to_string(),
            block_x: floor_to_i64(x)?,
            block_y: floor_to_i64(y)?,
            block_z: floor_to_i64(z)?,
        })
    }
}

impl From<&crate::waypoints::proto::Waypoint> for LocationKey {
    fn from(waypoint: &crate::waypoints::proto::Waypoint) -> Self {
        // A stored waypoint came through `validate`, so the floor is total here.
        LocationKey::of(
            &waypoint.dimension_id,
            waypoint.x,
            waypoint.y,
            waypoint.z,
        )
        .unwrap_or(LocationKey {
            dimension_id: waypoint.dimension_id.clone(),
            block_x: 0,
            block_y: 0,
            block_z: 0,
        })
    }
}

fn floor_to_i64(value: f64) -> Option<i64> {
    if !value.is_finite() {
        return None;
    }
    let floored = value.floor();
    if floored < i64::MIN as f64 || floored > i64::MAX as f64 {
        return None;
    }
    Some(floored as i64)
}

/// An untrusted create or update body, before validation.
#[derive(Clone, Debug, PartialEq)]
pub struct Draft {
    /// Display name.
    pub name: String,
    /// Dimension resource id.
    pub dimension_id: String,
    /// Block coordinates.
    pub x: f64,
    /// See [`Draft::x`].
    pub y: f64,
    /// See [`Draft::x`].
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

/// A validated body: trimmed, and with the marker style normalised.
#[derive(Clone, Debug, PartialEq)]
pub struct Validated {
    /// Trimmed display name.
    pub name: String,
    /// Dimension resource id, unchanged.
    pub dimension_id: String,
    /// Block coordinates, unchanged.
    pub x: f64,
    /// See [`Validated::x`].
    pub y: f64,
    /// See [`Validated::x`].
    pub z: f64,
    /// Colour, unchanged; the alpha was checked.
    pub color: i32,
    /// Kind, unchanged.
    pub kind: WaypointKind,
    /// Trimmed icon item id, or empty.
    pub icon_item_id: String,
    /// Trimmed marker label, or empty.
    pub marker_label: String,
}

/// Validates an untrusted body, returning the normalised form or `None`.
pub fn validate(draft: &Draft) -> Option<Validated> {
    let name = draft.name.trim().to_string();
    if !valid_display_text(&name, MAX_NAME_CODE_POINTS) {
        return None;
    }
    let dimension_id = draft.dimension_id.trim();
    if !valid_dimension_id(dimension_id) {
        return None;
    }
    for value in [draft.x, draft.y, draft.z] {
        if !value.is_finite() || value.abs() > MAX_ABSOLUTE_COORDINATE {
            return None;
        }
    }
    if draft.x.abs() > MAX_HORIZONTAL_COORDINATE || draft.z.abs() > MAX_HORIZONTAL_COORDINATE {
        return None;
    }
    // The client refuses to render a translucent marker, and so does the
    // reference server. Checking it here keeps the two in step.
    if (draft.color as u32 >> 24) != 0xff {
        return None;
    }
    Some(Validated {
        name,
        dimension_id: dimension_id.to_string(),
        x: draft.x,
        y: draft.y,
        z: draft.z,
        color: draft.color,
        kind: draft.kind,
        icon_item_id: normalize_icon_item_id(&draft.icon_item_id)?,
        marker_label: normalize_marker_label(&draft.marker_label)?,
    })
}

/// Whether a stored publisher name is still displayable.
///
/// A name reaches the catalog once, at publish time, and is then replayed to
/// every subscriber for as long as the point lives. A point whose publisher name
/// no longer passes - after a stricter rule, say - is quarantined on load rather
/// than shipped.
pub fn valid_publisher_name(value: &str) -> bool {
    valid_display_text(value.trim(), MAX_PUBLISHER_NAME_CODE_POINTS)
}

/// Display-text rule shared by names and labels: non-blank, within a code-point
/// budget, and free of control, formatting, and section-sign characters.
///
/// The section sign is rejected because it starts a legacy formatting code, so a
/// name containing one could recolour every line a client draws it on.
pub fn valid_display_text(value: &str, max_code_points: usize) -> bool {
    if value.trim().is_empty() || value.chars().count() > max_code_points {
        return false;
    }
    !value
        .chars()
        .any(|ch| ch.is_control() || ch == '\u{a7}' || is_format_char(ch))
}

/// Normalises a marker label, or `None` when it is unusable.
pub fn normalize_marker_label(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if !valid_display_text(trimmed, MAX_LABEL_CODE_POINTS) && !trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

/// Normalises a marker icon item id, or `None` when it is unusable.
///
/// Only vanilla item ids are accepted, because the id is resolved by the client
/// against its own item registry and a foreign namespace would silently render
/// as a default marker.
pub fn normalize_icon_item_id(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Some(String::new());
    }
    let path = trimmed.strip_prefix("minecraft:")?;
    if path.is_empty()
        || !path
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"/._-".contains(&b))
    {
        return None;
    }
    Some(trimmed.to_string())
}

/// Whether a dimension id is a resource location the client can index.
fn valid_dimension_id(value: &str) -> bool {
    if value.is_empty() || value.len() > 256 {
        return false;
    }
    match value.split_once(':') {
        Some((namespace, path)) => valid_resource_part(namespace, false) && valid_resource_part(path, true),
        // The client's own parser defaults a bare id into the `minecraft`
        // namespace, so one is acceptable here too.
        None => valid_resource_part(value, true),
    }
}

fn valid_resource_part(value: &str, allow_slash: bool) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'_' | b'-' | b'.')
                || (allow_slash && byte == b'/')
        })
}

/// Whether `ch` is a Unicode formatting character.
///
/// Java's `Character.getType(ch) == FORMAT` is the reference rule, and it needs
/// the full Unicode character database; this covers the ranges that can appear
/// in a name a player typed, which is what the rule is defending against (a
/// zero-width joiner, a bidirectional override, a byte-order mark). Being
/// narrower than Java here means accepting a few code points the reference
/// server would reject, never the other way round.
fn is_format_char(ch: char) -> bool {
    matches!(ch as u32,
        0x00ad
        | 0x0600..=0x0605
        | 0x061c
        | 0x06dd
        | 0x070f
        | 0x0890..=0x0891
        | 0x08e2
        | 0x180e
        | 0x200b..=0x200f
        | 0x202a..=0x202e
        | 0x2060..=0x2064
        | 0x2066..=0x206f
        | 0xfeff
        | 0xfff9..=0xfffb
        | 0x110bd
        | 0x110cd
        | 0x13430..=0x1343f
        | 0x1bca0..=0x1bca3
        | 0x1d173..=0x1d17a
        | 0xe0001
        | 0xe0020..=0xe007f
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft() -> Draft {
        Draft {
            name: "Base".to_string(),
            dimension_id: "minecraft:overworld".to_string(),
            x: 12.5,
            y: 64.0,
            z: -7.25,
            color: 0xff34_98dbu32 as i32,
            kind: WaypointKind::Normal,
            icon_item_id: "minecraft:compass".to_string(),
            marker_label: "B".to_string(),
        }
    }

    #[test]
    fn a_reasonable_draft_validates_and_is_trimmed() {
        let mut input = draft();
        input.name = "  Base  ".to_string();
        input.marker_label = "  B ".to_string();
        input.icon_item_id = " minecraft:compass ".to_string();
        let validated = validate(&input).expect("valid");
        assert_eq!(validated.name, "Base");
        assert_eq!(validated.marker_label, "B");
        assert_eq!(validated.icon_item_id, "minecraft:compass");
    }

    #[test]
    fn names_are_bounded_and_stripped_of_display_hazards() {
        assert!(!valid_display_text("", 64), "empty");
        assert!(!valid_display_text("   ", 64), "blank");
        assert!(valid_display_text(&"a".repeat(64), 64), "at the limit");
        assert!(!valid_display_text(&"a".repeat(65), 64), "over the limit");
        assert!(
            valid_display_text(&"长".repeat(64), 64),
            "the budget is code points, not bytes"
        );
        assert!(!valid_display_text("a\u{a7}cb", 64), "section sign");
        assert!(!valid_display_text("a\nb", 64), "control character");
        assert!(!valid_display_text("a\u{200b}b", 64), "zero-width space");
        assert!(!valid_display_text("a\u{202e}b", 64), "bidi override");
        assert!(valid_display_text("Ünïcödé 🗺", 64), "ordinary text passes");
    }

    #[test]
    fn a_label_is_at_most_three_code_points_and_may_be_absent() {
        assert_eq!(normalize_marker_label("").as_deref(), Some(""));
        assert_eq!(normalize_marker_label(" AB ").as_deref(), Some("AB"));
        assert_eq!(normalize_marker_label("abc").as_deref(), Some("abc"));
        assert_eq!(
            normalize_marker_label("\u{1f5fa}\u{fe0f}").as_deref(),
            Some("\u{1f5fa}\u{fe0f}"),
            "an emoji plus its variation selector is two code points"
        );
        assert_eq!(normalize_marker_label("abcd"), None);
        assert_eq!(normalize_marker_label("a\nb"), None);
    }

    #[test]
    fn an_icon_item_id_must_be_a_vanilla_item() {
        assert_eq!(normalize_icon_item_id("").as_deref(), Some(""));
        assert_eq!(
            normalize_icon_item_id("minecraft:diamond_sword").as_deref(),
            Some("minecraft:diamond_sword")
        );
        assert_eq!(normalize_icon_item_id("mymod:thing"), None);
        assert_eq!(normalize_icon_item_id("minecraft:"), None);
        assert_eq!(normalize_icon_item_id("minecraft:Diamond"), None);
    }

    #[test]
    fn coordinates_are_bounded_on_every_axis() {
        let mut input = draft();
        input.x = MAX_HORIZONTAL_COORDINATE + 1.0;
        assert!(validate(&input).is_none(), "horizontal bound");
        input = draft();
        input.z = -MAX_HORIZONTAL_COORDINATE - 1.0;
        assert!(validate(&input).is_none());
        input = draft();
        input.y = MAX_ABSOLUTE_COORDINATE + 1.0;
        assert!(validate(&input).is_none(), "vertical bound");
        input = draft();
        input.x = f64::NAN;
        assert!(validate(&input).is_none());
        input = draft();
        input.x = MAX_HORIZONTAL_COORDINATE;
        assert!(validate(&input).is_some(), "the bound itself is allowed");
    }

    #[test]
    fn a_colour_must_be_opaque() {
        let mut input = draft();
        input.color = 0x0034_98db;
        assert!(validate(&input).is_none(), "translucent markers are refused");
        input.color = 0xffff_ffffu32 as i32;
        assert!(validate(&input).is_some());
    }

    #[test]
    fn dimension_ids_follow_the_resource_location_rule() {
        let mut input = draft();
        for good in [
            "minecraft:overworld",
            "minecraft:the_nether",
            "minecraft:the_end",
            "mymod:arena/one",
            "overworld",
        ] {
            input.dimension_id = good.to_string();
            assert!(validate(&input).is_some(), "{good} should be accepted");
        }
        for bad in ["", "Minecraft:overworld", "minecraft:Overworld", "minecraft:the nether", ":x", "x:"] {
            input.dimension_id = bad.to_string();
            assert!(validate(&input).is_none(), "{bad} should be rejected");
        }
    }

    #[test]
    fn a_location_key_floors_every_axis_and_keeps_the_dimension_apart() {
        let here = LocationKey::of("minecraft:overworld", 12.9, 64.0, -7.25).expect("finite");
        let near = LocationKey::of("minecraft:overworld", 12.1, 64.9, -7.99).expect("finite");
        assert_eq!(here, near, "both points occupy block 12, 64, -8");
        assert_eq!((here.block_x, here.block_y, here.block_z), (12, 64, -8));
        let elsewhere = LocationKey::of("minecraft:the_nether", 12.9, 64.0, -7.25).expect("finite");
        assert_ne!(here, elsewhere, "the same block in another dimension is free");
        assert!(LocationKey::of("minecraft:overworld", f64::NAN, 0.0, 0.0).is_none());
        assert!(LocationKey::of("minecraft:overworld", f64::INFINITY, 0.0, 0.0).is_none());
    }
}
