# confluxmap-pumpkin

[![CI](https://github.com/Chonghua-05/confluxmap-pumpkin/actions/workflows/ci.yml/badge.svg)](https://github.com/Chonghua-05/confluxmap-pumpkin/actions/workflows/ci.yml)

[简体中文](README.md) | English

A companion plugin for [confluxmap](https://github.com/Conflux-Union/conflux-map) on the
[Pumpkin](https://github.com/Pumpkin-MC/Pumpkin) server.

## Upstream projects

| Project | Description |
|---|---|
| **confluxmap** | A Minecraft client mod together with its server-side companions (a Fabric server entrypoint, and plugins for Paper / Folia). The client generates a predicted map locally from the world seed, then asks the server for authoritative patches to correct prediction drift. This plugin implements the handshake of its `confluxmap:map_sync` channel. |
| **Pumpkin** | A Minecraft server written in Rust. Plugins load as `wasm32-wasip2` components, run inside a WASI sandbox, call host capabilities through WIT interfaces, and are authorized according to the permissions declared in their metadata. |

## Feature support

This plugin implements the **complete core companion suite** of confluxmap on Pumpkin: **automatic seed and version extraction from world archives**, **authoritative map corrections from Anvil MCA saves**, **entity radar stream (player coordinates broadcast)**, **view distance synchronization**, **server instance isolation**, and **shared waypoints**.

### Supported

| Capability | Notes |
|---|---|
| **Channel declaration** | Announces `confluxmap:map_sync` and `confluxmap:waypoints_v1` to the client with `minecraft:register` at login. |
| **Automatic Archive Seed & Version Discovery** | Aligned with Pumpkin's `stored_world_seed` cascade reader: automatically extracts seed and Minecraft version from mounted world saves (`world_gen_settings.dat` -> `level.dat`). |
| **Authoritative Map Corrections** | Answers `0x0C MAP_REGION_VIEW_REQ` and `0x03 MAP_VIEW_REQ`, reading `region/r.*.*.mca` via read-only Anvil parser and delivering region/tile patches. |
| **Entity Radar Stream** | 20-tick periodic broadcast of player coordinates (`0x14 PLAYER_POSITIONS`) with spectator and dimension tracking. |
| **View Distance Sync** | Negotiates and delivers server view distance (`0x11 SERVER_VIEW_DISTANCE`). |
| **Server Instance Identity** | Grants `0x13 SERVER_INSTANCE` (capability id 7) to differentiate sub-worlds sharing a seed behind proxies. |
| **Shared Waypoints** | The separate `confluxmap:waypoints_v1` channel (protocol 1.3): subscription, create, update, delete, broadcast, and atomic persistence. |
| **Rate Limiting & Hardening** | 3-strikes malformed packet muting and structured error frame (`0x06 ERROR`) response. |
| **Operator Commands** | `/cfm status`, `/cfm seed`, `/cfm hello`, `/cfm reload`, `/cfm waypoints (list/clear)`. |

### Not supported or left out by design

| Capability | Notes |
|---|---|
| **Web Map** | Upstream embeds an HTTP + WebSocket web server. Kept out to maintain a lightweight in-game companion focused on player sync. |
| **Chunk Load State Overlay** | `0x09 LOAD_STATE_DELTA`. Pumpkin WIT API currently does not expose an iterator for loaded chunks. |

The three restriction flags `biomeMapForbidden`, `structureSearchForbidden` and
`entityRadarForbidden` are never sent by this plugin, so the client keeps its default
behaviour.

The plugin-message channel exists on the Java edition only. A Bedrock client never starts the
handshake, and this plugin sends it nothing.

## Supported Pumpkin version

| Item | Value |
|---|---|
| Pinned plugin API | `pumpkin-plugin-api = 0.1.0-dev+26.2-26.45` |
| Corresponding server build | `0.1.0-dev+26.2-26.45` |
| Corresponding Minecraft | 26.2 (Java protocol 776) |
| Plugin form | `wasm32-wasip2` component; dropping it into `plugins/` is enough to load it |

## Installation

1. Get `confluxmap_pumpkin.wasm`: download it from
   [Releases](https://github.com/Chonghua-05/confluxmap-pumpkin/releases), or build it yourself
   as described in the next section.

2. Put the file into `plugins/` under the server's working directory.

3. Start the server once. On its first load the plugin writes an annotated configuration
   template into its own data directory:

   ```
   plugins/data/confluxmap-pumpkin/config.toml
   ```

4. In that file, uncomment the `seed` line and fill in this world's seed; it must match the
   top-level `seed` in `pumpkin.toml`:

4. **Providing the world seed (choose one):**
   - **Method A (Automatic archive discovery, recommended)**:
     Symlink the server's world directory into the plugin data directory:
     ```bash
     ln -s ../../world plugins/data/confluxmap-pumpkin/world
     ```
     On startup or `/cfm reload`, the plugin cascade-reads `world/data/minecraft/world_gen_settings.dat` or `world/level.dat` to discover the seed, worldgen version, and enable authoritative Anvil map corrections.
   - **Method B (Manual fallback)**:
     Uncomment `seed` in `config.toml` and fill it in:
     ```toml
     seed = 81985529216486895
     ```
     Then run `/cfm reload`.

5. Verify: `/cfm seed` should print the seed, the world ID and the worldgen version, and the
   load log should contain `auto-detected world archive` or `seed = ...` and `load complete`.

One point deserves attention during installation:

- **Permission configuration.** Pumpkin defaults to `ask_permission_confirmation = true`. This plugin requests `fs.read.data` and `fs.write.data`. On non-interactive servers, pre-approve them in `pumpkin.toml`:
  ```toml
  [plugins.overrides.confluxmap_pumpkin]
  enabled = true
  allowed_permissions = ["fs.read.data", "fs.write.data"]
  ```

## Building and testing

```bash
./build.sh                          # cargo build --release -> target/wasm32-wasip2/release/confluxmap_pumpkin.wasm
./test.sh                           # unit tests, including golden-vector tests aligned byte for byte with the reference Java encoder
python3 tools/test_inject_seed.py   # regression tests for the seed-writing script
```

`build.sh` and `test.sh` are wrappers that restore the MSVC environment under Windows / Git
Bash: there, the GNU `link` from Git Bash is resolved before MSVC `link.exe`, which makes rustc
fail to link. On Linux, macOS and CI, run `cargo build --release` and `cargo test` directly.

The tests run on the host with `--target x86_64-pc-windows-msvc`: the protocol and
configuration modules do not call the plugin API (the configuration tests exercise the parser
without touching the disk), and the bindings generated from the plugin API compile for a native
target as well, so there is no need to enter wasm.

## Operator commands

| Command | Description |
|---|---|
| `/cfm status` | Configuration summary, counters and the running server version |
| `/cfm seed` | Prints the seed, the world ID and the worldgen version currently advertised |
| `/cfm hello` | Replays the parsed result of the most recent handshakes, and the bytes of the last policy frame |
| `/cfm reload` | Re-reads the configuration file |
| `/cfm waypoints` | The state of the shared waypoint directory: the switch, the channel, the current revision and quotas, and where it is stored |
| `/cfm waypoints list [page]` | One page of the directory, six entries per page |
| `/cfm waypoints clear` | Empties the directory, emitting one `REMOVE` delta per point and persisting |

## Configuration file

`plugins/data/confluxmap-pumpkin/config.toml`, written by the plugin on its first load with
every key present as a comment, and `/cfm reload` re-reads all of them. The permissions the plugin requests from the host are
`fs.read.data` and `fs.write.data`, that is, that private directory only; it requests no
environment, network or other permission.

| Key | Required | Description |
|---|---|---|
| `seed` | yes | The server's world seed. Accepts a signed decimal integer, and also the `u64` bit-pattern spelling (for displayed values above `i64::MAX`); `_` is accepted as a digit separator. When it is unset the plugin still answers the handshake, but `seedGranted = 0` and the client shows no map |
| `share_seed` | no | When `false` the seed is withheld (`seedGranted = 0`) and the plugin still answers the handshake; defaults to `true` |
| `worldgen` | no | An explicit worldgen version string; by default derived from the server's version string |
| `world_id` | no | An explicit world ID; by default derived from the seed as `00000000-0000-0000-0000-<low 48 bits of the seed>` |
| `dims` | no | The dimension id list, written either as a comma-separated string or as a TOML array; by default `minecraft:overworld` only. Known vanilla dimensions are marked predictable, and anything else is treated as unpredictable |
| `share_waypoints` | no | Whether the shared-waypoint channel `confluxmap:waypoints_v1` is served at all; defaults to `true`. With `false` the catalogue is neither loaded nor changed, and every request is refused as disabled |
| `allow_non_operator_waypoint_management` | no | Whether non-operators may manage the points they published; defaults to `true`. With `false` only permission level ≥ 2 may change anything |
| `max_waypoints_per_world` | no | Cap on the total number of points in one world, at most 512; defaults to `512` |
| `max_waypoints_per_player` | no | Cap on the points one player may publish, at most the previous key; defaults to `64` |
| `waypoint_mutations_per_minute` | no | One player's mutation budget per minute (1-6000); defaults to `30` |

Parsing is lenient: one `key = value` per line, `#` starts a comment, and values may be quoted.
Values that cannot be parsed and unknown keys are only warned about and fall back to the
default; they never make the plugin fail to load.

### Persisted files

Besides `config.toml`, the plugin keeps two state files in the same private directory:

| File | Contents |
|---|---|
| `server_instance.json` | This instance's UUID as `{"uuid": "..."}`, the same shape as upstream's `UuidFileStore`. Generated and written on first use; regenerated and warned about when the file cannot be read |
| `shared_waypoints.json` | The shared waypoint catalogue, schema 2, in the same document shape as upstream. A corrupt file is quarantined as `.bad` and rebuilt; a document of a higher schema is left untouched with the feature disabled; a document carrying another server's `ownerInstanceId` is renamed to `.bak` |

## Layout

```
src/protocol.rs  Wire codec, a byte-for-byte mirror of confluxmap's MsgCodec.java
src/channel.rs   Declares the confluxmap:map_sync and confluxmap:waypoints_v1 channels to clients
src/config.rs    The plugin's configuration file (template, parsing, validation)
src/handshake.rs HELLO -> HELLO_POLICY reply logic
src/commands.rs  The /cfm command tree
src/state.rs     Configuration snapshot, counters and deduplication of channel announcements
src/identity.rs  Generating and persisting the server instance id
src/json.rs      Minimal JSON reading and writing, for the instance id and the waypoint document
src/wire.rs      Byte-order read and write primitives
src/clock.rs     Wall-clock time, for `createdAtEpochMs` and rate limiting
src/waypoints/   Shared waypoints
  proto.rs       Codec of confluxmap:waypoints_v1 (protocol 1.3)
  model.rs       Waypoint model and coordinate validation
  store.rs       Catalogue state, revision and quotas
  persist.rs     Reading and writing shared_waypoints.json, and quarantining corrupt files
  service.rs     Mutation decisions, deltas and rate limiting
  session.rs     Per-connection session state and malformed-packet muting
  mod.rs         Event wiring and broadcast
src/lib.rs       Plugin entry point and event registration
docs/protocol.md             Wire format of both channels, the handshake frame order and the client decision path (Chinese)
docs/pumpkin-capabilities.md Pumpkin host capability audit and per-feature feasibility (Chinese)
tools/PolicyVector.java      Generates the golden vector with the reference Java encoder
tools/inject_seed.py         Idempotently writes the plugin config from pumpkin.toml
tools/test_inject_seed.py    Regression tests for the seed-writing script
build.sh / test.sh           Build and test
```

## Licence

LGPL-3.0-or-later, see [LICENSE](LICENSE).
