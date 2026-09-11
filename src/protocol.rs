//! Wire codec for the `confluxmap:map_sync` companion channel.
//!
//! This is a byte-for-byte mirror of the Java reference implementation in
//! `common/src/main/java/cn/net/rms/confluxmap/core/net/MsgCodec.java`. Every
//! multi-byte value is big-endian; strings are a `u16` byte length followed by
//! raw UTF-8 bytes (`writeUtf` / `readUtf`).
//!
//! Four messages are implemented:
//!
//! * `0x01 C2S HELLO` - the client announces its versions, and - since protocol
//!   major 4 - carries a capability offer appended to `predictorVersion`.
//! * `0x12 S2C MAP_CAPABILITIES` - the server's capability selection, sent only
//!   to a client that offered one.
//! * `0x13 S2C SERVER_INSTANCE` - this server instance's id, sent only once the
//!   client has been granted the `SERVER_INSTANCE` capability.
//! * `0x02 S2C HELLO_POLICY` - the seed + worldgen version, corrections off.
//!
//! The client treats a policy with `correctionsEnabled = 0` as
//! `ClientMode.SERVER_DISABLED`: the session stays ACTIVE, the seed stays
//! usable, and the client simply never asks for authoritative patches.
//!
//! # Why the handshake is not one frame
//!
//! `SERVER_INSTANCE` is capability-gated, and a capability exists only because
//! the client offered it *and* the server selected it in `0x12`. A client that
//! receives `0x13` without a matching selection refuses it outright
//! (`NegotiatedMapSync.requireCapability`). So the order is fixed:
//! `0x12` selection, then `0x13`, then `0x02` - and the policy last, because
//! the client opens its session on the policy frame.

use crate::wire;

/// Registered plugin-messaging channel (`Proto.CHANNEL_ID`).
pub const CHANNEL_ID: &str = "confluxmap:map_sync";

/// `0x01 C2S HELLO` (`Proto.MSG_HELLO_C2S`).
pub const MSG_HELLO_C2S: u8 = 0x01;
/// `0x02 S2C HELLO_POLICY` (`Proto.MSG_HELLO_POLICY_S2C`).
pub const MSG_HELLO_POLICY_S2C: u8 = 0x02;
/// `0x12 S2C MAP_CAPABILITIES` (`Proto.MSG_MAP_CAPABILITIES_S2C`).
pub const MSG_MAP_CAPABILITIES_S2C: u8 = 0x12;
/// `0x13 S2C SERVER_INSTANCE` (`Proto.MSG_SERVER_INSTANCE_S2C`).
pub const MSG_SERVER_INSTANCE_S2C: u8 = 0x13;

/// Hard cap on any UTF-8 field (`Proto.MAX_UTF8_BYTES`).
pub const MAX_UTF8_BYTES: usize = 256;
/// Hard cap on per-dimension entries (`Proto.MAX_DIM_ENTRIES`).
pub const MAX_DIM_ENTRIES: usize = 8;

/// Negotiation envelope version (`MapSyncProtocol.NEGOTIATION_VERSION`).
pub const NEGOTIATION_VERSION: u8 = 2;
/// The marker that introduces a capability offer inside `predictorVersion`.
pub const CAPS_MARKER: &str = "|caps2:";
/// Cap on offered/selected capability entries (`MapSyncProtocol.MAX_CAPABILITIES`).
pub const MAX_CAPABILITIES: usize = 32;
/// Cap on offered correction profiles (`MapSyncProtocol.MAX_CORRECTION_PROFILES`).
pub const MAX_CORRECTION_PROFILES: usize = 8;

/// `MapSyncCapability.SERVER_INSTANCE`: the only capability this plugin grants.
///
/// The other seven are correction-related, and `PLAYER_POSITIONS` comes with the
/// radar stream. Granting one this plugin does not implement would leave the
/// client waiting for messages that never arrive, so they are withheld.
pub const CAP_SERVER_INSTANCE: u8 = 7;
/// `MapSyncCapability.SERVER_INSTANCE`'s version (`MapSyncCapability.version()`).
pub const CAP_SERVER_INSTANCE_VERSION: u8 = 1;
/// `MapCompatibilityS2C.MODE_DISABLED`.
pub const CORRECTION_MODE_DISABLED: u8 = 2;
/// `MapCompatibilityS2C.REASON_NO_COMMON_WIRE`.
pub const REASON_NO_COMMON_WIRE: u8 = 2;
/// `CorrectionProfile.SOURCE_LIGHT_V2.id()`.
///
/// Reported for shape compatibility only. With the mode disabled the client
/// never decodes a correction body, so this selects nothing concrete.
pub const CORRECTION_PROFILE_SOURCE_LIGHT_V2: u8 = 2;

/// The `HELLO_POLICY` flag byte's bit vocabulary, named so the wire contract is
/// complete and future correction-capable builds have the bits defined rather
/// than spelled as magic numbers.
///
/// This plugin sets [`flags::SEED_GRANTED`] and nothing else, i.e. the byte
/// `0x01`. That is not an invented shape: it is exactly what the reference Paper
/// companion emits through `CompanionPolicy.configuredFlags` when configured
/// with `shareSeed = true, shareCorrections = false` - every other bit already
/// defaults to 0 there, and zeroing `correctionsEnabled` zeroes the two
/// correction bits that gate on it. The client answers an `0x01` policy with
/// `ClientMode.SERVER_DISABLED`: session ACTIVE, seed usable, no corrections.
#[allow(dead_code)]
pub mod flags {
    /// A per-dim seed is present in the policy.
    pub const SEED_GRANTED: u8 = 1 << 0;
    /// The server will answer authoritative correction requests.
    ///
    /// Deliberately never set by this plugin: that is what makes it a
    /// seed-only companion rather than a full map-sync server.
    pub const CORRECTIONS_ENABLED: u8 = 1 << 1;
    /// The client must not use its biome map.
    pub const BIOME_MAP_FORBIDDEN: u8 = 1 << 2;
    /// The server pushes a chunk load-state overlay.
    pub const CHUNK_LOAD_STATE_ENABLED: u8 = 1 << 3;
    /// The client must not run the entity radar.
    pub const ENTITY_RADAR_FORBIDDEN: u8 = 1 << 4;
    /// The server notifies the client when corrections are invalidated.
    pub const CORRECTION_INVALIDATION_ENABLED: u8 = 1 << 5;
    /// The server serves cropped chunk-range correction pages.
    pub const CHUNK_RANGE_CORRECTION_ENABLED: u8 = 1 << 6;
    /// The client must not run structure search.
    pub const STRUCTURE_SEARCH_FORBIDDEN: u8 = 1 << 7;
}

const DIM_PREDICTABLE: u8 = 1 << 0;
const DIM_HAS_SEED: u8 = 1 << 1;
/// Generator preset occupies dim bits 2..=4 (`dimBits |= preset.wireId() << 2`).
const DIM_PRESET_SHIFT: u8 = 2;

/// Rate/batch limits advertised to the client (`HelloPolicyS2C.Budgets`).
///
/// A seed-only server answers no `MAP_VIEW_REQ`, so these are only a
/// declaration of intent; the client still reads them and they must be
/// non-degenerate (`maxTilesPerReq` of 0 would make the client's view planner
/// reject every request).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budgets {
    /// Advertised byte budget per second.
    pub max_bytes_per_sec: i32,
    /// Maximum tiles per `MAP_VIEW_REQ`.
    pub max_tiles_per_req: u16,
    /// Minimum interval between requests, in milliseconds.
    pub min_req_interval_ms: u16,
    /// Maximum level-of-detail the server will patch.
    pub max_patch_lod: u8,
}

impl Default for Budgets {
    /// Mirrors `Proto.DEFAULT_*`, so a plugin left at defaults is
    /// indistinguishable from a stock Paper server to the client.
    fn default() -> Self {
        Budgets {
            max_bytes_per_sec: 256 * 1024,
            max_tiles_per_req: 8,
            min_req_interval_ms: 100,
            max_patch_lod: 4,
        }
    }
}

/// One served dimension (`HelloPolicyS2C.DimDescriptor`).
///
/// Its index in the policy's dim list is the `dimIndex` the client echoes back
/// in later requests, so order is part of the wire contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dim {
    /// Stringified dimension id, e.g. `minecraft:overworld`.
    pub id: String,
    /// Vanilla dimension type: `overworld` / `the_nether` / `the_end`.
    pub kind: String,
    /// Whether the server can ever produce corrections for this dimension.
    /// With corrections disabled this is informational, but it tells the
    /// client which dimensions are worth predicting at all.
    pub predictable: bool,
    /// Whether [`Dim::seed`] is meaningful.
    pub has_seed: bool,
    /// World seed. Only sent when `has_seed` is set.
    pub seed: i64,
    /// `WorldPreset` wire id (0 = `DEFAULT`). Occupies dim bits 2..=4; a
    /// pre-preset client masks the bits away and reads `DEFAULT`.
    pub preset: u8,
}

impl Dim {
    fn bits(&self) -> u8 {
        let mut bits = 0u8;
        if self.predictable {
            bits |= DIM_PREDICTABLE;
        }
        if self.has_seed {
            bits |= DIM_HAS_SEED;
        }
        bits |= (self.preset & 0x7) << DIM_PRESET_SHIFT;
        bits
    }
}

/// Decoded `0x01 HELLO_C2S`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelloC2S {
    /// Friendly mod version, e.g. `0.2.0`.
    pub mod_version: String,
    /// Prediction-pipeline identity (cubiomes commit + shim ABI + baseline
    /// algo). The reference server compares this to decide whether residual
    /// coding is possible; a seed-only server can ignore it but should log it,
    /// because a mismatch is the first thing to check when a client's
    /// predicted map disagrees with the world.
    pub predictor_version: String,
}

/// Decodes a `0x01 HELLO_C2S`.
///
/// Returns `None` for anything that is not exactly one HELLO frame, including
/// a wrong type byte, a truncated field, invalid UTF-8, or **trailing bytes** -
/// the reference `MsgCodec.decode` rejects trailing bytes and this decoder
/// keeps that strictness so a desynced client cannot be misread.
pub fn parse_hello_c2s(data: &[u8]) -> Option<HelloC2S> {
    let mut r = wire::Reader::new(data);
    if r.u8()? != MSG_HELLO_C2S {
        return None;
    }
    let mod_version = r.utf(MAX_UTF8_BYTES)?;
    let predictor_version = r.utf(MAX_UTF8_BYTES)?;
    if r.remaining() != 0 {
        return None;
    }
    Some(HelloC2S {
        mod_version,
        predictor_version,
    })
}

/// What a client's HELLO said about its own capabilities.
///
/// A protocol-major-4 client appends a legacy advertisement and then a
/// `|caps2:`-introduced, base64url-encoded offer to `predictorVersion`. That
/// field is the *only* place a client can advertise anything: the HELLO frame's
/// shape is unchanged, so this data has to travel inside a string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    /// The predictor identity proper, with the appended advertisement removed.
    /// A server that shares a baseline compares this; this plugin only logs it.
    pub predictor: String,
    /// Whether the client wrapped its advertisement in a capability offer.
    pub caps2: bool,
    /// Whether the offer included `SERVER_INSTANCE` at a usable version.
    pub server_instance: bool,
}

/// Parses the `predictorVersion` field of a HELLO.
///
/// Every failure path mirrors `MapSyncProtocol.parseOffer`: a malformed offer
/// still reports `caps2`, because the client clearly believes it sent one and
/// answering it is how the two sides agree on *no* capabilities. Only a field
/// with no marker at all is treated as a legacy client.
pub fn parse_offer(field: &str) -> Offer {
    let predictor = match field.find("|sync:") {
        // Everything before the advertisement tokens is the predictor identity.
        Some(index) => &field[..index],
        None => field,
    };
    let Some(marker) = field.find(CAPS_MARKER) else {
        return Offer {
            predictor: predictor.to_string(),
            caps2: false,
            server_instance: false,
        };
    };
    let rest = &field[marker + CAPS_MARKER.len()..];
    let encoded = match rest.find('|') {
        Some(index) => &rest[..index],
        None => rest,
    };
    let capabilities = decode_offer(encoded);
    Offer {
        predictor: predictor.to_string(),
        caps2: true,
        server_instance: capabilities
            .iter()
            .any(|(id, version)| *id == CAP_SERVER_INSTANCE && *version > 0),
    }
}

/// Decodes the capability offer, returning the capabilities the client claims.
///
/// An empty result is the correct answer for anything structurally wrong: the
/// caller then selects no capabilities, which is a no-op for both sides.
fn decode_offer(encoded: &str) -> Vec<(u8, u8)> {
    let Some(bytes) = base64_url_decode(encoded) else {
        return Vec::new();
    };
    let mut r = wire::Reader::new(&bytes);
    let Some(version) = r.u8() else {
        return Vec::new();
    };
    if version != NEGOTIATION_VERSION {
        return Vec::new();
    }
    let Some(profile_count) = r.u8() else {
        return Vec::new();
    };
    if profile_count < 1 || usize::from(profile_count) > MAX_CORRECTION_PROFILES {
        return Vec::new();
    }
    let mut profiles = 0usize;
    for _ in 0..profile_count {
        if r.u8().is_none() {
            return Vec::new();
        }
        profiles += 1;
    }
    let Some(capability_count) = r.u8() else {
        return Vec::new();
    };
    if usize::from(capability_count) > MAX_CAPABILITIES {
        return Vec::new();
    }
    let mut offered: Vec<(u8, u8)> = Vec::new();
    for _ in 0..capability_count {
        let (Some(id), Some(version)) = (r.u8(), r.u8()) else {
            return Vec::new();
        };
        if version == 0 {
            continue;
        }
        match offered.iter_mut().find(|(seen, _)| *seen == id) {
            Some(entry) => entry.1 = entry.1.max(version),
            None => offered.push((id, version)),
        }
    }
    if r.remaining() != 0 || profiles == 0 {
        return Vec::new();
    }
    offered
}

/// Decodes unpadded base64url, tolerating `=` padding.
fn base64_url_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut accumulator: u32 = 0;
    let mut bits: u32 = 0;
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => u32::from(byte - b'A'),
            b'a'..=b'z' => u32::from(byte - b'a') + 26,
            b'0'..=b'9' => u32::from(byte - b'0') + 52,
            b'-' => 62,
            b'_' => 63,
            b'=' => continue,
            _ => return None,
        };
        accumulator = (accumulator << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((accumulator >> bits) as u8);
        }
    }
    Some(out)
}

/// Builds the `0x12 MAP_CAPABILITIES` selection.
///
/// The correction fields report the state this plugin actually keeps: no
/// correction profile is usable, so the mode is `DISABLED` with reason
/// `NO_COMMON_WIRE`. That is the same frame the reference server sends when the
/// client and server share no correction profile, and it is what makes the
/// client accept everything else in the envelope.
pub fn build_map_capabilities(server_mod_version: &str, capabilities: &[(u8, u8)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);
    out.push(MSG_MAP_CAPABILITIES_S2C);
    wire::u8(&mut out, NEGOTIATION_VERSION);
    write_utf(&mut out, server_mod_version);
    // The baseline predictor identity. This plugin does not claim one, and the
    // client only consults it for a residual correction stream it will not get.
    write_utf(&mut out, "");
    wire::u8(&mut out, CORRECTION_MODE_DISABLED);
    wire::u8(&mut out, REASON_NO_COMMON_WIRE);
    wire::u8(&mut out, CORRECTION_PROFILE_SOURCE_LIGHT_V2);
    let capabilities = &capabilities[..capabilities.len().min(MAX_CAPABILITIES)];
    wire::u8(&mut out, capabilities.len() as u8);
    for (id, version) in capabilities {
        wire::u8(&mut out, *id);
        wire::u8(&mut out, *version);
    }
    out
}

/// Builds the `0x13 SERVER_INSTANCE` frame.
pub fn build_server_instance(instance_id: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(48);
    out.push(MSG_SERVER_INSTANCE_S2C);
    write_utf(&mut out, instance_id);
    out
}

/// Mirrors `MsgCodec.writeUtf`.
///
/// The reference implementation *throws* on a field longer than
/// [`MAX_UTF8_BYTES`]. A server that throws would drop the whole handshake on a
/// misconfigured dimension name, so this clamps at a UTF-8 char boundary
/// instead and the caller logs the truncation.
fn write_utf(out: &mut Vec<u8>, s: &str) {
    let mut end = s.len().min(MAX_UTF8_BYTES);
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let bytes = &s.as_bytes()[..end];
    out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(bytes);
}

/// Builds the `0x02 HELLO_POLICY` reply.
///
/// The flag byte is assembled from `flags`; this plugin always passes
/// [`flags::SEED_GRANTED`] alone (or nothing when the seed is withheld), so
/// `correctionsEnabled` stays clear and the client parks in
/// `SERVER_DISABLED` while still using the seed.
///
/// `dims` is silently truncated to [`MAX_DIM_ENTRIES`], matching the encoder's
/// cap; the caller is expected to have validated it already.
pub fn build_hello_policy(
    flags: u8,
    world_id: &str,
    worldgen_version: &str,
    budgets: Budgets,
    dims: &[Dim],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);
    out.push(MSG_HELLO_POLICY_S2C);
    out.push(flags);
    write_utf(&mut out, world_id);
    write_utf(&mut out, worldgen_version);
    out.extend_from_slice(&budgets.max_bytes_per_sec.to_be_bytes());
    out.extend_from_slice(&budgets.max_tiles_per_req.to_be_bytes());
    out.extend_from_slice(&budgets.min_req_interval_ms.to_be_bytes());
    out.push(budgets.max_patch_lod);
    let dims = &dims[..dims.len().min(MAX_DIM_ENTRIES)];
    out.push(dims.len() as u8);
    for dim in dims {
        write_utf(&mut out, &dim.id);
        write_utf(&mut out, &dim.kind);
        out.push(dim.bits());
        out.extend_from_slice(&dim.seed.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A predictable vanilla overworld, for building test frames.
    fn overworld(seed: i64, has_seed: bool) -> Dim {
        Dim {
            id: "minecraft:overworld".to_string(),
            kind: "overworld".to_string(),
            predictable: true,
            has_seed,
            seed: if has_seed { seed } else { 0 },
            preset: 0,
        }
    }

    /// A representative 28-byte HELLO frame as a confluxmap client sends it
    /// (`modVersion = "0.2.0"`, `predictorVersion = "probe-predictor-v0"`).
    /// The parser only cares about the framing, so the field values are
    /// illustrative.
    const REAL_HELLO: &[u8] = &[
        0x01, 0x00, 0x05, b'0', b'.', b'2', b'.', b'0', 0x00, 0x12, b'p', b'r', b'o', b'b', b'e',
        b'-', b'p', b'r', b'e', b'd', b'i', b'c', b't', b'o', b'r', b'-', b'v', b'0',
    ];

    #[test]
    fn parses_a_hello_frame() {
        let hello = parse_hello_c2s(REAL_HELLO).expect("real HELLO must parse");
        assert_eq!(hello.mod_version, "0.2.0");
        assert_eq!(hello.predictor_version, "probe-predictor-v0");
    }

    #[test]
    fn rejects_wrong_type_and_trailing_bytes() {
        let mut wrong = REAL_HELLO.to_vec();
        wrong[0] = MSG_HELLO_POLICY_S2C;
        assert!(parse_hello_c2s(&wrong).is_none());

        let mut trailing = REAL_HELLO.to_vec();
        trailing.push(0x00);
        assert!(parse_hello_c2s(&trailing).is_none());
    }

    #[test]
    fn rejects_truncated_and_oversized_fields() {
        assert!(parse_hello_c2s(&[]).is_none());
        assert!(parse_hello_c2s(&[0x01]).is_none());
        // Length prefix claims more bytes than the buffer holds.
        assert!(parse_hello_c2s(&[0x01, 0x00, 0x40, b'x']).is_none());
        // Length prefix above MAX_UTF8_BYTES (0x0101 = 257).
        assert!(parse_hello_c2s(&[0x01, 0x01, 0x01]).is_none());
    }

    /// The flag bit assignments are a wire contract shared with the reference
    /// implementation; pin them so a refactor cannot silently renumber them.
    #[test]
    fn flag_bits_match_the_reference_definition() {
        assert_eq!(flags::SEED_GRANTED, 0x01);
        assert_eq!(flags::CORRECTIONS_ENABLED, 0x02);
        assert_eq!(flags::BIOME_MAP_FORBIDDEN, 0x04);
        assert_eq!(flags::CHUNK_LOAD_STATE_ENABLED, 0x08);
        assert_eq!(flags::ENTITY_RADAR_FORBIDDEN, 0x10);
        assert_eq!(flags::CORRECTION_INVALIDATION_ENABLED, 0x20);
        assert_eq!(flags::CHUNK_RANGE_CORRECTION_ENABLED, 0x40);
        assert_eq!(flags::STRUCTURE_SEARCH_FORBIDDEN, 0x80);
    }

    /// The authoritative 97-byte HELLO_POLICY, produced by the confluxmap
    /// reference encoder itself (`MsgCodec.encode`, via `tools/PolicyVector.java`),
    /// not by hand and not by this crate.
    ///
    /// Provenance of the *inputs*: the reference encoder was originally run with
    /// the seed of a live server, which cannot ship in a public repository. The
    /// vector therefore carries a synthetic seed, and it is derived - not
    /// regenerated - from that reference output: only the two seed-derived fields
    /// (`worldId`'s trailing 12 hex digits and the trailing 8-byte `seed`) were
    /// substituted. Every other byte is the reference encoder's own output.
    ///
    /// That substitution is exact rather than approximate, because `encode` is a
    /// straight field-by-field writer: `flags`, `worldgenVersion`, `budgets` and
    /// the dim descriptors do not depend on the seed, `worldId` is written as an
    /// opaque `utf` string of unchanged length (36 bytes), and the dim seed is a
    /// fixed-width big-endian `i64`. So the frame below is byte-for-byte what the
    /// reference encoder emits for these inputs. This test is consequently a real
    /// cross-implementation check - the expected bytes still originate from the
    /// Java implementation, not from the Rust one.
    const GOLDEN_POLICY_HEX: &str = "0201002430303030303030302d303030302d303030302d303030302d343536373839616263646566000432362e320004000000080064040100136d696e6563726166743a6f766572776f726c6400096f766572776f726c64030123456789abcdef";

    /// A deliberately synthetic seed (`0x0123456789ABCDEF`), so the vector
    /// carries no real world's seed.
    const GOLDEN_SEED: i64 = 0x0123_4567_89AB_CDEF;
    const GOLDEN_WORLD_ID: &str = "00000000-0000-0000-0000-456789abcdef";

    fn hex_to_bytes(hex: &str) -> Vec<u8> {
        (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).expect("valid hex"))
            .collect()
    }

    #[test]
    fn encoder_matches_the_reference_implementation_byte_for_byte() {
        let dims = [overworld(GOLDEN_SEED, true)];
        let policy = build_hello_policy(
            flags::SEED_GRANTED,
            GOLDEN_WORLD_ID,
            "26.2",
            Budgets::default(),
            &dims,
        );
        let golden = hex_to_bytes(GOLDEN_POLICY_HEX);
        assert_eq!(golden.len(), 97, "reference vector must be a full frame");
        assert_eq!(
            policy, golden,
            "Rust encoder diverged from confluxmap's MsgCodec.encode"
        );
    }

    #[test]
    fn withheld_seed_clears_the_granted_flag_and_zeroes_the_seed() {
        let dims = [overworld(GOLDEN_SEED, false)];
        assert_eq!(dims[0].seed, 0);
        assert_eq!(dims[0].bits(), DIM_PREDICTABLE);
        let policy = build_hello_policy(0, GOLDEN_WORLD_ID, "26.2", Budgets::default(), &dims);
        assert_eq!(
            policy[1], 0,
            "no flags may be set when the seed is withheld"
        );
    }

    #[test]
    fn dim_bits_carry_predictable_has_seed_and_preset() {
        let mut dim = overworld(GOLDEN_SEED, true);
        assert_eq!(dim.bits(), DIM_PREDICTABLE | DIM_HAS_SEED);
        dim.preset = 2; // AMPLIFIED
        assert_eq!(
            dim.bits(),
            DIM_PREDICTABLE | DIM_HAS_SEED | (2 << DIM_PRESET_SHIFT)
        );
    }

    #[test]
    fn overly_long_utf8_is_clamped_at_a_char_boundary() {
        // 300 bytes of a 3-byte character: the encoder must stop at 255, never
        // mid-character, or the client's decoder would reject the frame.
        let long = "\u{4e00}".repeat(100);
        let mut out = Vec::new();
        write_utf(&mut out, &long);
        let len = u16::from_be_bytes([out[0], out[1]]) as usize;
        assert_eq!(len, 255);
        assert!(core::str::from_utf8(&out[2..]).is_ok());
    }

    /// The offer a current client appends to `predictorVersion`, produced by the
    /// reference `MapSyncProtocol.encodeOffer`: negotiation envelope 2, profiles
    /// `MATERIAL_COLOR_V3, SOURCE_LIGHT_V2, LEGACY_V1`, then all eight
    /// capabilities at version 1.
    const REAL_OFFER: &str = "AgMDAgEIAQECAQMBBAEFAQYBBwEIAQ";

    /// The same client's full `predictorVersion` field, marker included.
    fn current_predictor_field(predictor: &str) -> String {
        format!(
            "{predictor}|sync:1|wire:4.0|patch:3|region:1|patch:4|region:2|source-light:1\
             |server-view:1|caps2:{REAL_OFFER}"
        )
    }

    #[test]
    fn a_current_client_offers_the_server_instance_capability() {
        let offer = parse_offer(&current_predictor_field("cubiomes-9f2c"));
        assert_eq!(offer.predictor, "cubiomes-9f2c");
        assert!(offer.caps2, "the caps2 marker must be recognised");
        assert!(
            offer.server_instance,
            "capability 7 is in the real offer, so it must be seen"
        );
    }

    #[test]
    fn a_legacy_client_offers_nothing() {
        // No advertisement at all: the whole field is the predictor identity.
        let plain = parse_offer("probe-predictor-v0");
        assert_eq!(plain.predictor, "probe-predictor-v0");
        assert!(!plain.caps2);
        assert!(!plain.server_instance);

        // Advertises negotiation support but not capabilities.
        let legacy = parse_offer("probe|sync:1|wire:4.0|server-view:1");
        assert_eq!(legacy.predictor, "probe");
        assert!(!legacy.caps2);
    }

    #[test]
    fn a_malformed_offer_still_counts_as_one() {
        // The client clearly meant to send an offer; the reference server
        // answers it with an empty selection instead of ignoring it.
        let broken = parse_offer("pred|caps2:$$$$$");
        assert!(broken.caps2);
        assert!(!broken.server_instance);

        // A structurally valid offer that simply does not include capability 7.
        let narrow = parse_offer("pred|caps2:AgEBAQEB");
        assert!(
            narrow.caps2,
            "envelope version 2 is enough to make it an offer"
        );
        assert!(!narrow.server_instance);

        // A different envelope version is not something we can read.
        let future = parse_offer("pred|caps2:AwEBAQEB");
        assert!(future.caps2);
        assert!(!future.server_instance);
    }

    #[test]
    fn the_advertisement_inside_the_field_is_not_part_of_the_predictor() {
        // Only the tokens before `|sync:` identify the predictor; logging the
        // whole field would put a base64 blob in every handshake line.
        let offer = parse_offer(&current_predictor_field("probe-predictor-v0"));
        assert_eq!(offer.predictor, "probe-predictor-v0");
    }

    #[test]
    fn the_selection_frame_matches_the_reference_shape() {
        let frame = build_map_capabilities("0.1.1", &[(CAP_SERVER_INSTANCE, 1)]);
        assert_eq!(
            crate::state::hex(&frame),
            "12020005302e312e310000020202010701"
        );
        assert_eq!(frame[0], MSG_MAP_CAPABILITIES_S2C);
    }

    #[test]
    fn an_empty_selection_is_still_a_well_formed_envelope() {
        let frame = build_map_capabilities("0.1.1", &[]);
        assert_eq!(crate::state::hex(&frame), "12020005302e312e31000002020200");
        // The capability count is the last byte.
        assert_eq!(*frame.last().expect("non-empty"), 0);
    }

    #[test]
    fn the_instance_frame_carries_the_id_verbatim() {
        let frame = build_server_instance(GOLDEN_WORLD_ID);
        assert_eq!(frame[0], MSG_SERVER_INSTANCE_S2C);
        assert_eq!(
            crate::state::hex(&frame),
            "13002430303030303030302d303030302d303030302d303030302d343536373839616263646566"
        );
        let mut r = wire::Reader::new(&frame[1..]);
        assert_eq!(r.utf(MAX_UTF8_BYTES).as_deref(), Some(GOLDEN_WORLD_ID));
        assert_eq!(r.remaining(), 0);
    }

    #[test]
    fn base64_url_decoding_rejects_foreign_characters() {
        assert_eq!(
            base64_url_decode("AgMDAgEIAQECAQMBBAEFAQYBBwEIAQ").map(|b| b.len()),
            Some(22)
        );
        assert!(base64_url_decode("AgMDAgEIAQECAQMBBAEFAQYBBwEIAQ=").is_some());
        assert_eq!(base64_url_decode("!!!!"), None);
        assert_eq!(base64_url_decode(""), Some(Vec::new()));
    }
}
