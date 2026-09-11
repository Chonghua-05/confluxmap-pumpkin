//! Wire codec for the `confluxmap:map_sync` companion channel.
//!
//! This is a byte-for-byte mirror of the Java reference implementation in
//! `common/src/main/java/cn/net/rms/confluxmap/core/net/MsgCodec.java`. Every
//! multi-byte value is big-endian; strings are a `u16` byte length followed by
//! raw UTF-8 bytes (`writeUtf` / `readUtf`).
//!
//! Only the two messages the v1 server needs are implemented:
//!
//! * `0x01 C2S HELLO` - the client announces its versions.
//! * `0x02 S2C HELLO_POLICY` - the server answers with the seed + worldgen
//!   version and explicitly disables corrections.
//!
//! The client treats a policy with `correctionsEnabled = 0` as
//! `ClientMode.SERVER_DISABLED`: the session stays ACTIVE, the seed stays
//! usable, and the client simply never asks for authoritative patches.

/// Registered plugin-messaging channel (`Proto.CHANNEL_ID`).
pub const CHANNEL_ID: &str = "confluxmap:map_sync";

/// `0x01 C2S HELLO` (`Proto.MSG_HELLO_C2S`).
pub const MSG_HELLO_C2S: u8 = 0x01;
/// `0x02 S2C HELLO_POLICY` (`Proto.MSG_HELLO_POLICY_S2C`).
pub const MSG_HELLO_POLICY_S2C: u8 = 0x02;

/// Hard cap on any UTF-8 field (`Proto.MAX_UTF8_BYTES`).
pub const MAX_UTF8_BYTES: usize = 256;
/// Hard cap on per-dimension entries (`Proto.MAX_DIM_ENTRIES`).
pub const MAX_DIM_ENTRIES: usize = 8;

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

/// Bounds-checked big-endian cursor over an untrusted payload.
///
/// Every read returns `Option`; a truncated or oversized field is a clean
/// `None` rather than a panic, because the payload arrives from the network.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    fn u8(&mut self) -> Option<u8> {
        let v = *self.buf.get(self.pos)?;
        self.pos += 1;
        Some(v)
    }

    fn u16(&mut self) -> Option<u16> {
        let hi = u16::from(*self.buf.get(self.pos)?);
        let lo = u16::from(*self.buf.get(self.pos + 1)?);
        self.pos += 2;
        Some((hi << 8) | lo)
    }

    /// Mirrors `MsgCodec.readUtf`: `u16` byte length, capped at
    /// [`MAX_UTF8_BYTES`], then that many UTF-8 bytes.
    fn utf(&mut self) -> Option<String> {
        let len = usize::from(self.u16()?);
        if len > MAX_UTF8_BYTES {
            return None;
        }
        let slice = self.buf.get(self.pos..self.pos + len)?;
        self.pos += len;
        core::str::from_utf8(slice).ok().map(str::to_owned)
    }

    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }
}

/// Decodes a `0x01 HELLO_C2S`.
///
/// Returns `None` for anything that is not exactly one HELLO frame, including
/// a wrong type byte, a truncated field, invalid UTF-8, or **trailing bytes** -
/// the reference `MsgCodec.decode` rejects trailing bytes and this decoder
/// keeps that strictness so a desynced client cannot be misread.
pub fn parse_hello_c2s(data: &[u8]) -> Option<HelloC2S> {
    let mut r = Reader::new(data);
    if r.u8()? != MSG_HELLO_C2S {
        return None;
    }
    let mod_version = r.utf()?;
    let predictor_version = r.utf()?;
    if r.remaining() != 0 {
        return None;
    }
    Some(HelloC2S {
        mod_version,
        predictor_version,
    })
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
}
