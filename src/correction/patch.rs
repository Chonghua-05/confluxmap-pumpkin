//! Wire codecs for Map Patch and Region Patch messages.
//!
//! Mirrors the ConfluxMap protocol specifications:
//! - `0x03 MSG_MAP_VIEW_REQ_C2S`: Requests tile patches
//! - `0x04 MSG_MAP_PATCH_S2C`: Server-to-client tile patch
//! - `0x0C MSG_MAP_REGION_VIEW_REQ_C2S`: Requests cropped region patches
//! - `0x0D MSG_MAP_REGION_PATCH_S2C`: Server-to-client region patch

use crate::wire;

/// Unchanged: peer already holds this exact revision or no updates exist.
pub const PATCH_MODE_UNCHANGED: u8 = 0;
/// Residual: sparse delta against the client-side predicted baseline.
pub const PATCH_MODE_RESIDUAL: u8 = 1;
/// Absolute: standalone surface/light records without prediction baseline.
pub const PATCH_MODE_ABSOLUTE: u8 = 2;
/// Unavailable: server cannot compute corrections for this region/tile.
pub const PATCH_MODE_UNAVAILABLE: u8 = 3;
/// Partial: progressive correction update with ongoing scan.
pub const PATCH_MODE_PARTIAL: u8 = 4;

/// Protocol message IDs matching `Proto.java`.
pub const MSG_MAP_VIEW_REQ_C2S: u8 = 0x03;
pub const MSG_MAP_PATCH_S2C: u8 = 0x04;
pub const MSG_MAP_REGION_VIEW_REQ_C2S: u8 = 0x0C;
pub const MSG_MAP_REGION_PATCH_S2C: u8 = 0x0D;

/// Tile presence bitmap size in bytes (16x16 / 8 = 32 bytes).
pub const PATCH_PRESENCE_BYTES: usize = 32;

/// Maximum compressed byte size allowed for a patch payload body (576 KiB).
pub const MAX_COMPRESSED_BYTES: usize = 576 * 1024;

/// Hard limits matching reference protocol.
pub const MAX_TILES_PER_REQ: usize = 8;
pub const MAX_REGION_PAGES_PER_REQ: usize = 32;
pub const MAX_DIM_ENTRIES: usize = 8;
pub const MAX_LOD: u8 = 4;

/// An individual tile request entry in `0x03 MSG_MAP_VIEW_REQ_C2S`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileReq {
    pub tile_x: i32,
    pub tile_z: i32,
    pub since_revision: i64,
}

/// Decoded `0x03 MSG_MAP_VIEW_REQ_C2S` message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapViewReqC2S {
    pub req_id: i32,
    pub dim_index: u8,
    pub lod: u8,
    pub tiles: Vec<TileReq>,
}

/// An individual region page request entry in `0x0C MSG_MAP_REGION_VIEW_REQ_C2S`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionReq {
    pub region_x: i32,
    pub region_z: i32,
    pub min_local_chunk_x: u8,
    pub min_local_chunk_z: u8,
    pub max_local_chunk_x: u8,
    pub max_local_chunk_z: u8,
    pub since_revision: i64,
}

/// Decoded `0x0C MSG_MAP_REGION_VIEW_REQ_C2S` message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapRegionViewReqC2S {
    pub req_id: i32,
    pub dim_index: u8,
    pub lod: u8,
    pub regions: Vec<RegionReq>,
}

/// Structure overlay entry in `0x04 MSG_MAP_PATCH_S2C`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructureEntry {
    pub struct_type: u8,
    pub block_x: i32,
    pub block_z: i32,
    pub state: u8,
}

/// Decodes `0x03 MSG_MAP_VIEW_REQ_C2S` from a payload slice.
///
/// Format:
/// - `req_id: i32`
/// - `dim_index: u8`
/// - `lod: u8`
/// - `tile_count: u8` (must be <= `MAX_TILES_PER_REQ`)
/// - repeated `tile_count` times:
///   - `tile_x: i32`
///   - `tile_z: i32`
///   - `since_revision: i64`
pub fn decode_map_view_req_c2s(payload: &[u8]) -> Option<MapViewReqC2S> {
    let mut r = wire::Reader::new(payload);
    let type_id = r.u8()?;
    if type_id != MSG_MAP_VIEW_REQ_C2S {
        return None;
    }
    let req_id = r.i32()?;
    let dim_index = r.u8()?;
    let lod = r.u8()?;
    if dim_index as usize >= MAX_DIM_ENTRIES || lod > MAX_LOD {
        return None;
    }
    let tile_count = r.u8()? as usize;
    if tile_count > MAX_TILES_PER_REQ {
        return None;
    }
    let mut tiles = Vec::with_capacity(tile_count);
    for _ in 0..tile_count {
        tiles.push(TileReq {
            tile_x: r.i32()?,
            tile_z: r.i32()?,
            since_revision: r.i64()?,
        });
    }
    if r.remaining() != 0 {
        return None;
    }
    Some(MapViewReqC2S {
        req_id,
        dim_index,
        lod,
        tiles,
    })
}

/// Decodes `0x0C MSG_MAP_REGION_VIEW_REQ_C2S` from a payload slice.
///
/// Format:
/// - `req_id: i32`
/// - `dim_index: u8`
/// - `lod: u8`
/// - `region_count: u8` (1..=32)
/// - repeated `region_count` times:
///   - `region_x: i32`
///   - `region_z: i32`
///   - `min_local_chunk_x: u8`
///   - `min_local_chunk_z: u8`
///   - `max_local_chunk_x: u8`
///   - `max_local_chunk_z: u8`
///   - `since_revision: i64`
pub fn decode_map_region_view_req_c2s(payload: &[u8]) -> Option<MapRegionViewReqC2S> {
    let mut r = wire::Reader::new(payload);
    let type_id = r.u8()?;
    if type_id != MSG_MAP_REGION_VIEW_REQ_C2S {
        return None;
    }
    let req_id = r.i32()?;
    let dim_index = r.u8()?;
    let lod = r.u8()?;
    if dim_index as usize >= MAX_DIM_ENTRIES || lod > MAX_LOD {
        return None;
    }
    let region_count = r.u8()? as usize;
    if region_count == 0 || region_count > MAX_REGION_PAGES_PER_REQ {
        return None;
    }
    let mut regions = Vec::with_capacity(region_count);
    for _ in 0..region_count {
        let region_x = r.i32()?;
        let region_z = r.i32()?;
        let min_x = r.u8()?;
        let min_z = r.u8()?;
        let max_x = r.u8()?;
        let max_z = r.u8()?;
        let since_revision = r.i64()?;

        // Validate slice bounds: 0 <= min <= max < 16
        if min_x > max_x || min_z > max_z || max_x >= 16 || max_z >= 16 {
            return None;
        }

        regions.push(RegionReq {
            region_x,
            region_z,
            min_local_chunk_x: min_x,
            min_local_chunk_z: min_z,
            max_local_chunk_x: max_x,
            max_local_chunk_z: max_z,
            since_revision,
        });
    }
    if r.remaining() != 0 {
        return None;
    }
    Some(MapRegionViewReqC2S {
        req_id,
        dim_index,
        lod,
        regions,
    })
}

/// Encodes `0x0D MSG_MAP_REGION_PATCH_S2C`.
///
/// Header + layout:
/// - `0x0D: u8`
/// - `req_id: i32`
/// - `dim_index: u8`
/// - `lod: u8`
/// - `region_x: i32`
/// - `region_z: i32`
/// - `min_local_chunk_x: u8`
/// - `min_local_chunk_z: u8`
/// - `max_local_chunk_x: u8`
/// - `max_local_chunk_z: u8`
/// - `mode: u8`
/// - `region_revision: i64`
/// - `body: bounded bytes (i32 len + raw bytes)`
#[allow(clippy::too_many_arguments)]
pub fn encode_map_region_patch_s2c(
    req_id: i32,
    dim_index: u8,
    lod: u8,
    region_x: i32,
    region_z: i32,
    min_local_chunk_x: u8,
    min_local_chunk_z: u8,
    max_local_chunk_x: u8,
    max_local_chunk_z: u8,
    mode: u8,
    region_revision: i64,
    body: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(30 + body.len());
    out.push(MSG_MAP_REGION_PATCH_S2C);
    wire::i32(&mut out, req_id);
    wire::u8(&mut out, dim_index);
    wire::u8(&mut out, lod);
    wire::i32(&mut out, region_x);
    wire::i32(&mut out, region_z);
    wire::u8(&mut out, min_local_chunk_x);
    wire::u8(&mut out, min_local_chunk_z);
    wire::u8(&mut out, max_local_chunk_x);
    wire::u8(&mut out, max_local_chunk_z);
    wire::u8(&mut out, mode);
    wire::i64(&mut out, region_revision);

    // writeBoundedBytes: i32 len + payload
    wire::i32(&mut out, body.len() as i32);
    out.extend_from_slice(body);
    out
}

/// Encodes `0x04 MSG_MAP_PATCH_S2C`.
///
/// Header + layout:
/// - `0x04: u8`
/// - `req_id: i32`
/// - `dim_index: u8`
/// - `lod: u8`
/// - `tile_x: i32`
/// - `tile_z: i32`
/// - `mode: u8`
/// - `tile_revision: i64`
/// - `presence: [u8; 32]`
/// - `struct_count: u8` (0 by default when omitted)
/// - repeated `struct_count` times:
///   - `struct_type: u8`
///   - `block_x: i32`
///   - `block_z: i32`
///   - `state: u8`
/// - `body: bounded bytes (i32 len + raw bytes)`
#[allow(clippy::too_many_arguments)]
pub fn encode_map_patch_s2c(
    req_id: i32,
    dim_index: u8,
    lod: u8,
    tile_x: i32,
    tile_z: i32,
    mode: u8,
    tile_revision: i64,
    presence: &[u8; PATCH_PRESENCE_BYTES],
    body: &[u8],
) -> Vec<u8> {
    encode_map_patch_s2c_with_structures(
        req_id,
        dim_index,
        lod,
        tile_x,
        tile_z,
        mode,
        tile_revision,
        presence,
        &[],
        body,
    )
}

/// Encodes `0x04 MSG_MAP_PATCH_S2C` with explicit structure overlay entries.
#[allow(clippy::too_many_arguments)]
pub fn encode_map_patch_s2c_with_structures(
    req_id: i32,
    dim_index: u8,
    lod: u8,
    tile_x: i32,
    tile_z: i32,
    mode: u8,
    tile_revision: i64,
    presence: &[u8; PATCH_PRESENCE_BYTES],
    structures: &[StructureEntry],
    body: &[u8],
) -> Vec<u8> {
    let struct_count = structures.len().min(255);
    let mut out = Vec::with_capacity(55 + struct_count * 10 + body.len());
    out.push(MSG_MAP_PATCH_S2C);
    wire::i32(&mut out, req_id);
    wire::u8(&mut out, dim_index);
    wire::u8(&mut out, lod);
    wire::i32(&mut out, tile_x);
    wire::i32(&mut out, tile_z);
    wire::u8(&mut out, mode);
    wire::i64(&mut out, tile_revision);
    out.extend_from_slice(presence);

    wire::u8(&mut out, struct_count as u8);
    for s in &structures[..struct_count] {
        wire::u8(&mut out, s.struct_type);
        wire::i32(&mut out, s.block_x);
        wire::i32(&mut out, s.block_z);
        wire::u8(&mut out, s.state);
    }

    wire::i32(&mut out, body.len() as i32);
    out.extend_from_slice(body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_map_view_req_valid() {
        let mut raw = Vec::new();
        raw.push(MSG_MAP_VIEW_REQ_C2S);
        wire::i32(&mut raw, 0x12345678);
        wire::u8(&mut raw, 1); // dim
        wire::u8(&mut raw, 2); // lod
        wire::u8(&mut raw, 2); // tile count
        wire::i32(&mut raw, -1);
        wire::i32(&mut raw, -2);
        wire::i64(&mut raw, 0);
        wire::i32(&mut raw, 100);
        wire::i32(&mut raw, -100);
        wire::i64(&mut raw, 1_700_000_000);

        let decoded = decode_map_view_req_c2s(&raw).expect("decode failed");
        assert_eq!(decoded.req_id, 0x12345678);
        assert_eq!(decoded.dim_index, 1);
        assert_eq!(decoded.lod, 2);
        assert_eq!(decoded.tiles.len(), 2);
        assert_eq!(decoded.tiles[0], TileReq {
            tile_x: -1,
            tile_z: -2,
            since_revision: 0,
        });
        assert_eq!(decoded.tiles[1], TileReq {
            tile_x: 100,
            tile_z: -100,
            since_revision: 1_700_000_000,
        });
    }

    #[test]
    fn decode_map_view_req_rejects_oversized_count() {
        let mut raw = Vec::new();
        raw.push(MSG_MAP_VIEW_REQ_C2S);
        wire::i32(&mut raw, 1);
        wire::u8(&mut raw, 0);
        wire::u8(&mut raw, 0);
        wire::u8(&mut raw, (MAX_TILES_PER_REQ + 1) as u8); // 9 > 8

        assert_eq!(decode_map_view_req_c2s(&raw), None);
    }

    #[test]
    fn decode_map_region_view_req_valid() {
        let mut raw = Vec::new();
        raw.push(MSG_MAP_REGION_VIEW_REQ_C2S);
        wire::i32(&mut raw, 42);
        wire::u8(&mut raw, 0);
        wire::u8(&mut raw, 4);
        wire::u8(&mut raw, 1); // region count
        wire::i32(&mut raw, -2);
        wire::i32(&mut raw, 3);
        wire::u8(&mut raw, 14);
        wire::u8(&mut raw, 1);
        wire::u8(&mut raw, 15);
        wire::u8(&mut raw, 12);
        wire::i64(&mut raw, 123456);

        let decoded = decode_map_region_view_req_c2s(&raw).expect("decode failed");
        assert_eq!(decoded.req_id, 42);
        assert_eq!(decoded.dim_index, 0);
        assert_eq!(decoded.lod, 4);
        assert_eq!(decoded.regions.len(), 1);
        assert_eq!(decoded.regions[0], RegionReq {
            region_x: -2,
            region_z: 3,
            min_local_chunk_x: 14,
            min_local_chunk_z: 1,
            max_local_chunk_x: 15,
            max_local_chunk_z: 12,
            since_revision: 123456,
        });
    }

    #[test]
    fn decode_map_region_view_req_rejects_invalid_bounds() {
        let mut raw = Vec::new();
        raw.push(MSG_MAP_REGION_VIEW_REQ_C2S);
        wire::i32(&mut raw, 42);
        wire::u8(&mut raw, 0);
        wire::u8(&mut raw, 0);
        wire::u8(&mut raw, 1);
        wire::i32(&mut raw, 0);
        wire::i32(&mut raw, 0);
        wire::u8(&mut raw, 15); // min > max
        wire::u8(&mut raw, 0);
        wire::u8(&mut raw, 14);
        wire::u8(&mut raw, 0);
        wire::i64(&mut raw, 0);

        assert_eq!(decode_map_region_view_req_c2s(&raw), None);
    }

    #[test]
    fn encode_map_region_patch_roundtrip() {
        let body = [10, 20, 30, 40, 50];
        let encoded = encode_map_region_patch_s2c(
            99,
            0,
            1,
            -5,
            10,
            2,
            3,
            14,
            15,
            PATCH_MODE_RESIDUAL,
            9876543210,
            &body,
        );

        assert_eq!(encoded[0], MSG_MAP_REGION_PATCH_S2C);
        let mut r = wire::Reader::new(&encoded[1..]);
        assert_eq!(r.i32(), Some(99));
        assert_eq!(r.u8(), Some(0));
        assert_eq!(r.u8(), Some(1)); // lod
        assert_eq!(r.i32(), Some(-5));
        assert_eq!(r.i32(), Some(10));
        assert_eq!(r.u8(), Some(2));
        assert_eq!(r.u8(), Some(3));
        assert_eq!(r.u8(), Some(14));
        assert_eq!(r.u8(), Some(15));
        assert_eq!(r.u8(), Some(PATCH_MODE_RESIDUAL));
        assert_eq!(r.i64(), Some(9876543210));
        assert_eq!(r.i32(), Some(5)); // body length
        assert_eq!(r.remaining(), 5);
    }

    #[test]
    fn encode_map_patch_roundtrip() {
        let presence = [0xAAu8; PATCH_PRESENCE_BYTES];
        let body = [1, 2, 3, 4];
        let encoded = encode_map_patch_s2c(
            777,
            1,
            0,
            12,
            -34,
            PATCH_MODE_ABSOLUTE,
            1122334455,
            &presence,
            &body,
        );

        assert_eq!(encoded[0], MSG_MAP_PATCH_S2C);
        let mut r = wire::Reader::new(&encoded[1..]);
        assert_eq!(r.i32(), Some(777));
        assert_eq!(r.u8(), Some(1));
        assert_eq!(r.u8(), Some(0));
        assert_eq!(r.i32(), Some(12));
        assert_eq!(r.i32(), Some(-34));
        assert_eq!(r.u8(), Some(PATCH_MODE_ABSOLUTE));
        assert_eq!(r.i64(), Some(1122334455));

        // presence (32 bytes)
        for _ in 0..PATCH_PRESENCE_BYTES {
            assert_eq!(r.u8(), Some(0xAA));
        }

        // struct count = 0
        assert_eq!(r.u8(), Some(0));

        // body len = 4
        assert_eq!(r.i32(), Some(4));
        assert_eq!(r.remaining(), 4);
    }
}
