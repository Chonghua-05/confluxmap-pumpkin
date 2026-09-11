# confluxmap-pumpkin

[![CI](https://github.com/Chonghua-05/confluxmap-pumpkin/actions/workflows/ci.yml/badge.svg)](https://github.com/Chonghua-05/confluxmap-pumpkin/actions/workflows/ci.yml)

[简体中文](README.md) | English

A companion plugin for [confluxmap](https://github.com/Chonghua-05/conflux-map) on the
[Pumpkin](https://github.com/Pumpkin-MC/Pumpkin) server.

## Upstream projects

| Project | Description |
|---|---|
| **confluxmap** | A Minecraft client mod together with its server-side companions (a Fabric server entrypoint, and plugins for Paper / Folia). The client generates a predicted map locally from the world seed, then asks the server for authoritative patches to correct prediction drift. This plugin implements the handshake of its `confluxmap:map_sync` channel. |
| **Pumpkin** | A Minecraft server written in Rust. Plugins load as `wasm32-wasip2` components, run inside a WASI sandbox, call host capabilities through WIT interfaces, and are authorized according to the permissions declared in their metadata. |

## Feature support

This plugin implements the usable form of confluxmap on Pumpkin: **it hands out
the seed**, the client renders the predicted map locally, and authoritative corrections stay
disabled. Shared waypoints and the server instance identity were added in v0.1.1. The tables
below split the feature surface by whether a feature can be built on
Pumpkin at all; the criteria and the per-item evidence, including Pumpkin source locations,
are in [docs/pumpkin-capabilities.md](docs/pumpkin-capabilities.md) (in Chinese).

### Supported

| Capability | Notes |
|---|---|
| Channel declaration | Announces `confluxmap:map_sync` and `confluxmap:waypoints_v1` to the client with `minecraft:register` at login. A Paper server sends that on the plugin's behalf; Pumpkin has no such API, and without the announcement the client never starts the handshake |
| Handshake reply | Answers a `HELLO` with `HELLO_POLICY`; a client that advertises capabilities also receives `0x12` / `0x13` first. The handshake completes in a single round trip and no further messages follow |
| World seed | Sets `seedGranted = 1` and carries the seed per dimension |
| worldgen version | Lets the client select terrain generation parameters |
| World ID | Used by the client as the namespace of its map cache |
| Dimension list | Predictability and generator preset per dimension; `minecraft:overworld` only by default |
| Budgets | Carries the `Budgets` field in the policy. On that channel this plugin answers nothing but `HELLO`; the field exists only so that the client's parser does not see a degenerate value |
| Seed sharing switch | With `share_seed = false` the policy is sent as `seedGranted = 0` instead |
| Corrections disabled | `correctionsEnabled = 0`, which puts the client into `SERVER_DISABLED`: the session stays ACTIVE and the seed stays usable, but no authoritative patches are requested |
| Server instance identity | Granting `SERVER_INSTANCE` (capability id 7) to a client that advertises a capability offer in `predictorVersion`; the handshake replies with `0x13` carrying this instance's UUID, which the client uses as its storage namespace to tell apart several sub-worlds that share one worldId behind a Velocity proxy |
| Shared waypoints | The separate `confluxmap:waypoints_v1` channel (protocol 1.3): subscription, create, update, delete and broadcast, with upstream semantics |
| Payload validation | Strict decoding: the type byte, the UTF-8 length cap, and the requirement that the frame be consumed exactly |
| Operator commands | `/cfm status`, `/cfm seed`, `/cfm hello`, `/cfm reload`, `/cfm waypoints` |

The frame order behind the instance identity, the capability negotiation and the waypoint
message semantics are in [docs/protocol.md](docs/protocol.md). Two points differ from
upstream: the instance id and the waypoint catalogue are persisted in the plugin's private
data directory (the WASI sandbox opens only that directory; the world save directory is out
of reach), and waypoint height validation reuses the client's own coordinate bounds
(|coord| ≤ 30 000 000, horizontal ≤ 29 999 984) because Pumpkin exposes no per-dimension
height limits to plugins.

### Not supported: the host is capable, the plugin does not implement it

These are buildable on Pumpkin; they are simply outside the scope of the current form.

| Capability | Notes |
|---|---|
| Player position broadcast | The online-player position stream behind the entity radar; needs a per-tick task and player position reads |
| View distance | `SERVER_VIEW_DISTANCE` |
| Mid-session policy update | `POLICY_UPDATE`, changing the policy while a session is live |
| Capability negotiation (corrections) | `MAP_CAPABILITIES` (`0x12`) is used only to grant `SERVER_INSTANCE`; no correction capability is negotiated, and `MAP_COMPATIBILITY` (`0x10`) is never sent |
| Web map | Upstream serves it over HTTP and WebSocket. Pumpkin lets a plugin listen on TCP (WASI sockets, requiring the `network.tcp.bind` permission) but provides no HTTP server interface, so the protocols would have to be implemented by hand; upstream tiles come from the correction service, which reads the world save, so with corrections unavailable it would only recompute the client's own seed prediction in the browser, which is why it is left undone |
| Structured errors | The `ERROR` frame |
| Rate limiting and hardening | Token buckets, malformed-packet strikes and muting, idempotent mutation caching. The waypoint channel implements the whole set; the main channel only shakes hands and needs none of it |
| Administrative surface | The upstream `enable` / `disable` / `performance` commands |

### Not supported: no viable path on Pumpkin

The full shape of confluxmap is "predict, then correct". The correction half cannot hold on
Pumpkin:

| Capability | What is missing |
|---|---|
| Authoritative map corrections | `MAP_PATCH` / `MAP_REGION_PATCH`. A plugin cannot read the region files of the world save and cannot reach unloaded chunks, so no authoritative map can be produced |
| Correction invalidation | No chunk-dirty event covers every source of change; the block place and break events only cover player actions |
| Chunk load state | The plugin API cannot enumerate loaded chunks, and the chunk load and unload events have no dispatch site in the server |
| Superflat baseline | The world generator preset cannot be read |
| Automatic seed discovery | The plugin API exposes no seed accessor, and the sandbox opens only the plugin's private directory (see "Configuration file") |

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

   ```toml
   seed = 81985529216486895
   ```

   Then run `/cfm reload` to apply it immediately.

   Optional: `tools/inject_seed.py` reads the top-level `seed` from `pumpkin.toml` and writes
   it into that file, which prevents the two values from drifting apart:

   ```bash
   python3 tools/inject_seed.py /path/to/pumpkin.toml
   ```

5. Verify: `/cfm seed` should print the seed, the world ID and the worldgen version, and the
   load log should contain `seed = ...` and `load complete`.

One point deserves attention during installation:

- **Permission confirmation.** Pumpkin defaults to `ask_permission_confirmation = true`, so on
  its first load the plugin lists the permissions it requests (`fs.read.data`,
  `fs.write.data`) on the console and waits for confirmation; when the console is not
  interactive the request is treated as a denial and the plugin does not load. On a server
  started without a console, either set `[plugins] ask_permission_confirmation = false`, or
  pre-approve the permissions this plugin requests in `allowed_permissions`. Decisions are
  recorded per plugin file hash in `plugins/permission_cache.json`, so an updated plugin file
  is confirmed again.

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
