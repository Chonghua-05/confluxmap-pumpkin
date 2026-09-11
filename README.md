# confluxmap-pumpkin

[![CI](https://github.com/Chonghua-05/confluxmap-pumpkin/actions/workflows/ci.yml/badge.svg)](https://github.com/Chonghua-05/confluxmap-pumpkin/actions/workflows/ci.yml)

简体中文 | [English](README.en.md)

[confluxmap](https://github.com/Chonghua-05/conflux-map) 在
[Pumpkin](https://github.com/Pumpkin-MC/Pumpkin) 服务端上的伴侣插件。

## 上游项目

| 项目 | 说明 |
|---|---|
| **confluxmap** | Minecraft 客户端 mod 及其服务端伴侣（Fabric 服务端入口，以及面向 Paper / Folia 的插件）。它在客户端以世界种子本地生成预测地图，再向服务端请求权威补丁以校正预测偏差。本插件实现其 `confluxmap:map_sync` 通道的握手。 |
| **Pumpkin** | 以 Rust 实现的 Minecraft 服务端。插件以 `wasm32-wasip2` 组件形态加载，运行于 WASI 沙箱内，通过 WIT 接口调用宿主能力，并按插件元数据中声明的权限获得授权。 |

## 功能支持

本插件实现 confluxmap 在 Pumpkin 上的可用形态：**下发种子**，由客户端在本地生成
预测地图；权威纠错保持关闭。公共路径点与子世界标识已在 v0.1.1 一并实现。下表按
「是否做得出来」分三档；判定方法与逐条证据（含 Pumpkin 源码位置）见
[docs/pumpkin-capabilities.md](docs/pumpkin-capabilities.md)。

### 已支持

| 能力 | 说明 |
|---|---|
| 通道声明 | 登录时以 `minecraft:register` 向客户端宣告 `confluxmap:map_sync` 与 `confluxmap:waypoints_v1`。Paper 由服务端代发，Pumpkin 无此 API，不宣告则客户端不会发起握手 |
| 握手应答 | 收到 `HELLO` 后应答 `HELLO_POLICY`；带能力报价的客户端在此之前还会收到 `0x12` / `0x13`。握手在单次往返内完成，其后不再有往来消息 |
| 世界种子 | 置 `seedGranted = 1`，逐维度附带种子 |
| worldgen 版本 | 客户端据此选择地形生成参数 |
| 世界 ID | 客户端用作地图缓存的命名空间 |
| 维度列表 | 逐维度给出可预测性与生成器 preset；默认仅 `minecraft:overworld` |
| 限流预算声明 | 策略中携带 `Budgets` 字段。该通道上插件除 `HELLO` 外不应答任何请求，该字段仅用于满足客户端解析不得退化的要求 |
| 种子共享开关 | 置 `share_seed = false` 时改发 `seedGranted = 0` |
| 关闭权威纠错 | `correctionsEnabled = 0`，客户端据此进入 `SERVER_DISABLED`：会话保持 ACTIVE、种子可用，但不请求权威补丁 |
| 子世界标识 | 对在 `predictorVersion` 里声明能力报价的客户端授予 `SERVER_INSTANCE`（能力 id 7），握手回复 `0x13` 携带本实例 UUID；客户端以此为存储命名空间，从而区分 Velocity 之后共用同一 worldId 的多个子世界 |
| 公共路径点 | 独立通道 `confluxmap:waypoints_v1`（协议 1.3）：订阅、创建、修改、删除与广播，语义同上游 |
| 载荷校验 | 严格解码：类型字节、UTF-8 长度上限、整帧必须恰好消费完 |
| 运维命令 | `/cfm status`、`/cfm seed`、`/cfm hello`、`/cfm reload`、`/cfm waypoints` |

子世界标识与公共路径点的握手帧序、能力协商与消息语义见
[docs/protocol.md](docs/protocol.md)。两处与上游的差异：实例 id 与路径点均持久化在
插件私有数据目录（WASI 沙箱只开放该目录，插件读不到世界存档目录）；路径点的高度校验
沿用客户端的坐标边界（|coord| ≤ 3000 万、水平 ≤ 29999984），因为 Pumpkin 未向插件暴露
世界高度上下限。

### 未支持：宿主能力具备，本插件尚未实现

这些在 Pumpkin 上做得出来，只是不在当前形态的范围内。

| 能力 | 说明 |
|---|---|
| 玩家位置广播 | 实体雷达所需的在线玩家位置流，依赖每 tick 任务与玩家位置读取 |
| 视距下发 | `SERVER_VIEW_DISTANCE` |
| 策略热更 | `POLICY_UPDATE`，在会话中途变更策略 |
| 能力协商（纠错） | `MAP_CAPABILITIES`（`0x12`）只用于授予 `SERVER_INSTANCE`，不协商纠错能力；`MAP_COMPATIBILITY`（`0x10`）不下发 |
| 网页地图 | 上游是 HTTP + WebSocket 服务。Pumpkin 允许插件监听 TCP（WASI sockets，需申请 `network.tcp.bind`），但没有 HTTP 服务端接口，协议须自行实现；且上游瓦片来自读存档的纠错服务，此处只能退化为浏览器端按种子预测——在权威纠错不成立的前提下，这只是把客户端已有的本地预测重算一遍，因此不做 |
| 结构化错误 | `ERROR` 帧 |
| 限流与防护 | 令牌桶、畸形包 strike 与静音、变更幂等缓存。公共路径点通道已按上游实现；主通道只做握手，不涉及 |
| 运维管理面 | 上游的 `enable` / `disable` / `performance` 等命令 |

### 未支持：Pumpkin 侧无可行路径

confluxmap 的完整形态是「预测 → 校正」。校正这一半在 Pumpkin 上无法成立：

| 能力 | 缺失之处 |
|---|---|
| 权威地图纠错 | `MAP_PATCH` / `MAP_REGION_PATCH`。插件读不到世界存档的 region 文件，也拿不到未加载区块，无法生成权威地图 |
| 纠错失效广播 | 缺少覆盖全部变更来源的区块脏标记事件；区块放置与破坏事件只覆盖玩家行为 |
| 区块加载状态 | 插件接口无法枚举已加载区块；区块加载与卸载事件在服务端无派发点 |
| 超平坦基线 | 读不到世界生成器 preset |
| 种子自动获取 | 插件接口未提供种子访问器，沙箱只开放插件私有目录（见「配置文件」） |

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

4. 在该文件中取消 `seed` 一行的注释并填入本世界的种子，取值须与 `pumpkin.toml` 顶层的 `seed` 一致：

   ```toml
   seed = 81985529216486895
   ```

   随后执行 `/cfm reload` 即时生效。

   [可选]`tools/inject_seed.py` 会读取 `pumpkin.toml` 顶层的 `seed` 写入该文件，可避免两处取值不一致：

   ```bash
   python3 tools/inject_seed.py /path/to/pumpkin.toml
   ```

5. 核对：`/cfm seed` 应显示种子、世界 ID 与 worldgen 版本；加载日志中应出现 `seed = ...` 与 `load complete`。

安装时需注意：

- **权限确认。** Pumpkin 默认 `ask_permission_confirmation = true`，插件首次加载会在控制台列出其申请的权限（`fs.read.data`、`fs.write.data`）并等待确认；控制台不可交互时按拒绝处理，插件不会加载。以无控制台方式启动的服务端，请设置 `[plugins] ask_permission_confirmation = false`，或将该插件申请的权限列入 `allowed_permissions` 预先批准。已作出的决定按插件文件哈希记录于 `plugins/permission_cache.json`，插件文件更新后需重新确认。

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
