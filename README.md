# confluxmap-pumpkin

[![CI](https://github.com/Chonghua-05/confluxmap-pumpkin/actions/workflows/ci.yml/badge.svg)](https://github.com/Chonghua-05/confluxmap-pumpkin/actions/workflows/ci.yml)

简体中文 | [English](README.en.md)

[confluxmap](https://github.com/Conflux-Union/conflux-map) 在
[Pumpkin](https://github.com/Pumpkin-MC/Pumpkin) 服务端上的伴侣插件。

## 上游项目

| 项目 | 说明 |
|---|---|
| **confluxmap** | Minecraft 客户端 mod 及其服务端伴侣（Fabric 服务端入口，以及面向 Paper / Folia 的插件）。它在客户端以世界种子本地生成预测地图，再向服务端请求权威补丁以校正预测偏差。本插件实现其 `confluxmap:map_sync` 通道的握手。 |
| **Pumpkin** | 以 Rust 实现的 Minecraft 服务端。插件以 `wasm32-wasip2` 组件形态加载，运行于 WASI 沙箱内，通过 WIT 接口调用宿主能力，并按插件元数据中声明的权限获得授权。 |

## 功能支持

本插件完整实现 confluxmap 在 Pumpkin 上的**全套核心功能**：包括**自动从存档提取种子与版本**、**基于 Anvil MCA 存档的权威地图纠错**、**实体雷达（玩家坐标广播流）**、**视距同步**、**子世界实例隔离**以及**共享公共路标**。

### 已支持特性

| 能力 | 说明 |
|---|---|
| **通道声明** | 登录时以 `minecraft:register` 向客户端宣告 `confluxmap:map_sync` 与 `confluxmap:waypoints_v1`。避免 Pumpkin 默认不向客户端预宣告通道导致握手受阻 |
| **官方级联世界存档读取** | 对齐 Pumpkin `stored_world_seed` 级联探测器：从挂载的世界存档自动读取种子与版本（优先 `world_gen_settings.dat`，次选 `level.dat`），免去手动填写 |
| **权威地图纠错** | 响应客户端 `0x0C MAP_REGION_VIEW_REQ` 与 `0x03 MAP_VIEW_REQ` 请求，只读解析世界存档下的 `region/r.*.*.mca`（Anvil 格式），自动比对版本并回送纠错切片与瓦片 |
| **实体雷达流** | 20-tick 周期扫描在线玩家并广播维度/坐标（`0x14 PLAYER_POSITIONS`），严格区分旁观者与对应世界名称 |
| **视距同步** | 握手时协商下发服务端真实视距或配置覆盖视距（`0x11 SERVER_VIEW_DISTANCE`） |
| **子世界标识** | 协商下发 `0x13 SERVER_INSTANCE`（能力 id 7），以持久化实例 UUID 作为存储命名空间，区分多子世界缓存 |
| **公共路径点** | 独立通道 `confluxmap:waypoints_v1`（协议 1.3）：支持增删改查、增量广播推送与防损坏原子持久化 |
| **网络防护与限流** | 收到异常/超长畸形包累计 3 次后自动静音，防止网络泛洪，并回送 `0x06 ERROR` 错误码 |
| **运维命令** | `/cfm status`、`/cfm seed`、`/cfm hello`、`/cfm reload`、`/cfm waypoints (list/clear)` |

### 未支持或暂不实现项

| 能力 | 说明 |
|---|---|
| **网页地图** | 上游是内置 HTTP + WebSocket Web 地图服务。本插件保持轻量化 companion 定位，优先保障服内与客户端的地图与路标同步，不包含独立 Web 服务器 |
| **区块加载状态覆盖** | `0x09 LOAD_STATE_DELTA`。Pumpkin WIT 接口目前无全局枚举已加载区块的公开接口 |

`biomeMapForbidden`、`structureSearchForbidden`、`entityRadarForbidden` 三个限制标志本插件一律不下发，客户端维持其默认行为。

插件消息通道仅存在于 Java 版。Bedrock 客户端不会发起握手，本插件亦不向其发送数据。

## 适用的 Pumpkin 版本

| 项 | 值 |
|---|---|
| 锁定的插件 API | `pumpkin-plugin-api = 0.1.0-dev+26.2-26.45` |
| 对应服务端 build | `0.1.0-dev+26.2-26.45` |
| 对应 Minecraft | 26.2（Java 协议 776） |
| 插件形态 | `wasm32-wasip2` 组件，置于 `plugins/` 目录即可加载 |

## 安装

1. 取得 `confluxmap_pumpkin.wasm`：从
   [Releases](https://github.com/Chonghua-05/confluxmap-pumpkin/releases) 下载，或按下一节自行构建。

2. 将该文件放入服务端运行目录下的 `plugins/`。

3. 启动服务端一次。插件首次加载会在自身数据目录写出带注释的配置模板：

   ```
   plugins/data/confluxmap-pumpkin/config.toml
   ```

4. **世界种子提供方式（二选一）：**
   - **方式 A（自动从存档读取，推荐）**：
     将服务端的世界存档目录软链接或挂载到插件数据目录的 `world` 下：
     ```bash
     ln -s ../../world plugins/data/confluxmap-pumpkin/world
     ```
     插件启动或重载时会自动级联读取 `world/data/minecraft/world_gen_settings.dat` 或 `world/level.dat`，自动提取真实种子和生成版本。
   - **方式 B（手动配置兜底）**：
     在 `config.toml` 中取消 `seed` 一行的注释并填入本世界的种子：
     ```toml
     seed = 81985529216486895
     ```
     随后执行 `/cfm reload` 即时生效。

5. 核对：`/cfm seed` 应显示种子、世界 ID 与 worldgen 版本；加载日志中应出现 `auto-detected world archive` 或 `seed = ...` 与 `load complete`。

安装时需注意：

- **权限确认与配置。** Pumpkin 默认 `ask_permission_confirmation = true`，插件加载需要 `fs.read.data` 与 `fs.write.data` 权限。建议在服务端的 `pumpkin.toml` 中预先批准，避免非交互式控制台下被拒绝：
  ```toml
  # 在 pumpkin.toml 中为本插件预先配置权限：
  [plugins.overrides.confluxmap_pumpkin]
  enabled = true
  allowed_permissions = ["fs.read.data", "fs.write.data"]
  ```

## 构建与测试

```bash
./build.sh                          # cargo build --release -> target/wasm32-wasip2/release/confluxmap_pumpkin.wasm
./test.sh                           # 单元测试，含与参考 Java 编码器逐字节对齐的黄金向量测试
python3 tools/test_inject_seed.py   # 种子写入脚本的回归测试
```

`build.sh` 与 `test.sh` 是 Windows / Git Bash 下重建 MSVC 环境的包装脚本：Git Bash 的 GNU `link` 会先于 MSVC `link.exe` 被解析，导致 rustc 链接失败。Linux、macOS 与 CI 上直接执行 `cargo build --release` 与 `cargo test` 即可。

测试以 `--target x86_64-pc-windows-msvc` 在宿主上运行：协议与配置模块不调用插件 API（配置测试直接验证解析器，不触碰磁盘），插件 API 生成的绑定亦可编译至原生 target，因此无需进入 wasm。

## 运维命令

| 命令 | 说明 |
|---|---|
| `/cfm status` | 配置摘要、计数器与运行中的服务端版本 |
| `/cfm seed` | 显示当前下发的种子、世界 ID 与 worldgen 版本 |
| `/cfm hello` | 回放最近若干次握手的解析结果，以及最后一帧策略的字节 |
| `/cfm reload` | 重新读取配置文件 |
| `/cfm waypoints` | 公共路径点的状态：开关、通道、当前 revision 与配额、存储位置 |
| `/cfm waypoints list [page]` | 分页列出路径点，每页 6 条 |
| `/cfm waypoints clear` | 清空路径点目录，逐点产生 `REMOVE` 增量并落盘 |

## 配置文件

`plugins/data/confluxmap-pumpkin/config.toml`，首次加载时由插件写出，全部键均以注释形式给出，`/cfm reload` 即时重读。插件向宿主申请的权限为 `fs.read.data` 与 `fs.write.data`，即只限该私有目录，不申请环境变量、网络或其他权限。

| 键 | 必填 | 说明 |
|---|---|---|
| `seed` | 是 | 服务端世界种子。接受十进制有符号整数，亦接受按 `u64` 位模式书写的形式（用于大于 `i64::MAX` 的显示值），允许 `_` 作为数字分隔符。未填写时插件仍会应答握手，但 `seedGranted = 0`，客户端不显示地图 |
| `share_seed` | 否 | 设为 `false` 时不下发种子（`seedGranted = 0`），插件仍会应答握手；缺省 `true` |
| `worldgen` | 否 | 显式指定 worldgen 版本串；缺省由服务端版本串解析 |
| `world_id` | 否 | 显式指定世界 ID；缺省由种子派生为 `00000000-0000-0000-0000-<种子低 48 位>` |
| `dims` | 否 | 维度 id 列表，可写逗号分隔的字符串或 TOML 数组；缺省仅 `minecraft:overworld`。已知原版维度标记为可预测，其余按不可预测处理 |
| `share_waypoints` | 否 | 是否服务公共路径点通道 `confluxmap:waypoints_v1`；缺省 `true`。置 `false` 时目录不再载入与变更，请求一律以「功能已禁用」驳回 |
| `allow_non_operator_waypoint_management` | 否 | 是否允许非 op 管理自己发布的路径点；缺省 `true`。置 `false` 时仅 op 等级 ≥ 2 者可变更 |
| `max_waypoints_per_world` | 否 | 单个世界的路径点总数上限，≤ 512；缺省 `512` |
| `max_waypoints_per_player` | 否 | 单个玩家发布的路径点上限，≤ 上一项；缺省 `64` |
| `waypoint_mutations_per_minute` | 否 | 单个玩家的变更配额（每分钟，1–6000）；缺省 `30` |

解析规则宽松：每行一个 `key = value`，`#` 起注释，值可加引号。无法解析的值与未知的键只会在日志中告警并保留缺省值，不会导致插件加载失败。

### 持久化文件

同一私有目录下还有两个由插件维护的状态文件：

| 文件 | 内容 |
|---|---|
| `server_instance.json` | 本实例的 UUID，形如 `{"uuid": "..."}`，与上游 `UuidFileStore` 同形。首次使用时生成并写入；文件不可读时重新生成并告警 |
| `shared_waypoints.json` | 公共路径点目录，schema 2，文档形状与上游一致。损坏的文件隔离为 `.bad` 后重建；schema 更高的文档保留不动且功能置为不可用；带着别的服务端 `ownerInstanceId` 的文档改名为 `.bak` |

## 目录

```
src/protocol.rs  线协议编解码，逐字节镜像 confluxmap 的 MsgCodec.java
src/channel.rs   向客户端声明 confluxmap:map_sync 与 confluxmap:waypoints_v1 通道
src/config.rs    插件配置文件（模板、解析与校验）
src/handshake.rs HELLO -> HELLO_POLICY 应答逻辑
src/commands.rs  /cfm 命令树
src/state.rs     配置快照、计数器与通道声明的去重
src/identity.rs  服务端实例 id 的生成与持久化
src/json.rs     最小 JSON 读写（实例 id 与路径点文档的落盘）
src/wire.rs     字节序读写原语
src/clock.rs    墙钟时间源，供 createdAtEpochMs 与限流使用
src/waypoints/   公共路径点
  proto.rs       confluxmap:waypoints_v1 消息编解码（协议 1.3）
  model.rs       路径点模型与坐标校验
  store.rs       目录状态、revision 与配额
  persist.rs     shared_waypoints.json 的读写与损坏隔离
  service.rs     变更裁决、增量与限流
  session.rs     单连接会话状态与畸形包静音
  mod.rs         事件接入与广播
src/lib.rs       Plugin 入口与事件注册
docs/protocol.md             两个通道的线格式、握手帧序与客户端判定链路
docs/pumpkin-capabilities.md Pumpkin 宿主能力核查与逐项功能可行性
tools/PolicyVector.java      以参考 Java 编码器生成黄金向量
tools/inject_seed.py         由 pumpkin.toml 幂等写入插件配置
tools/test_inject_seed.py    种子写入脚本的回归测试
build.sh / test.sh           构建与测试
```

## 许可

LGPL-3.0-or-later，见 [LICENSE](LICENSE)。
