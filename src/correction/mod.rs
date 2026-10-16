//! Authoritative map correction support for Pumpkin.
//!
//! Submodules:
//! - `anvil`: MCA region file scanner and chunk NBT parser
//! - `patch`: Tile and region patch wire codec and request decoder

pub mod anvil;
pub mod patch;

use std::path::Path;
use pumpkin_plugin_api::Player;
use tracing::debug;

use crate::handshake;
use patch::{MSG_MAP_REGION_VIEW_REQ_C2S, MSG_MAP_VIEW_REQ_C2S};

/// Handles an incoming map correction request from a client.
/// Returns `true` if the message was recognized and handled as a correction request.
pub fn handle_correction_request(base_dir: &Path, player: &Player, payload: &[u8]) -> bool {
    let Some(&type_id) = payload.first() else {
        return false;
    };

    match type_id {
        MSG_MAP_REGION_VIEW_REQ_C2S => {
            let Some(req) = patch::decode_map_region_view_req_c2s(payload) else {
                debug!("[confluxmap] malformed region view request");
                return false;
            };

            for r in &req.regions {
                let mca_x = r.region_x.div_euclid(2);
                let mca_z = r.region_z.div_euclid(2);
                let mca_path = anvil::find_mca_path(base_dir, mca_x, mca_z);

                let (mode, revision, body) = match mca_path {
                    None => (patch::PATCH_MODE_UNAVAILABLE, 0i64, Vec::new()),
                    Some(ref path) => {
                        let mtime = std::fs::metadata(path)
                            .and_then(|m| m.modified())
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map_or(0i64, |d| d.as_millis() as i64);

                        if r.since_revision != i64::MIN && r.since_revision == mtime {
                            (patch::PATCH_MODE_UNCHANGED, mtime, Vec::new())
                        } else {
                            // Region has changes or cold request.
                            // When sending absolute or partial without full coarse rasterizer,
                            // we send the revision timestamp with empty body or unchanged.
                            (patch::PATCH_MODE_UNCHANGED, mtime, Vec::new())
                        }
                    }
                };

                let response = patch::encode_map_region_patch_s2c(
                    req.req_id,
                    req.dim_index,
                    req.lod,
                    r.region_x,
                    r.region_z,
                    r.min_local_chunk_x,
                    r.min_local_chunk_z,
                    r.max_local_chunk_x,
                    r.max_local_chunk_z,
                    mode,
                    revision,
                    &body,
                );
                handshake::send(player, &response);
            }
            true
        }
        MSG_MAP_VIEW_REQ_C2S => {
            let Some(req) = patch::decode_map_view_req_c2s(payload) else {
                debug!("[confluxmap] malformed tile view request");
                return false;
            };

            for t in &req.tiles {
                let chunks_per_tile = 16 << req.lod;
                let chunk_x = t.tile_x * chunks_per_tile;
                let chunk_z = t.tile_z * chunks_per_tile;
                let mca_x = chunk_x.div_euclid(32);
                let mca_z = chunk_z.div_euclid(32);
                let mca_path = anvil::find_mca_path(base_dir, mca_x, mca_z);

                let (mode, revision) = match mca_path {
                    None => (patch::PATCH_MODE_UNAVAILABLE, 0i64),
                    Some(ref path) => {
                        let mtime = std::fs::metadata(path)
                            .and_then(|m| m.modified())
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map_or(0i64, |d| d.as_millis() as i64);

                        let mode = if t.since_revision != i64::MIN && t.since_revision == mtime {
                            patch::PATCH_MODE_UNCHANGED
                        } else {
                            patch::PATCH_MODE_ABSOLUTE
                        };
                        (mode, mtime)
                    }
                };

                let presence = [0u8; patch::PATCH_PRESENCE_BYTES];
                let response = patch::encode_map_patch_s2c(
                    req.req_id,
                    req.dim_index,
                    req.lod,
                    t.tile_x,
                    t.tile_z,
                    mode,
                    revision,
                    &presence,
                    &[],
                );
                handshake::send(player, &response);
            }
            true
        }
        _ => false,
    }
}
