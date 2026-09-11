# Pumpkin 宿主能力核查与功能可行性

本文回答一个问题：confluxmap 服务端伴侣的每一项功能，在 Pumpkin 上是否**做得出来**。
结论按"宿主能力"而非"本插件是否愿意实现"划分，因此既包含已经实现的部分，也包含
宿主能力具备但本插件尚未实现的部分，以及**有能力但判断上暂不实现**的部分。
v0.1.1 起公共路径点与子世界标识已在测试虚拟机上实测落地，相关条目直接写实测结果。

## 判定方法

Pumpkin 的插件是 `wasm32-wasip2` 组件，只能用 WIT 暴露的接口，且需在插件元数据的
`permissions` 列表里声明权限。判定一项功能可行与否，必须同时满足两个条件：

1. WIT 接口有声明（插件侧看得见）；
2. 服务端的 Rust 代码里有**真实派发点或实现**（事件被 `fire`、函数有调用者）。

第二个条件不是形式主义：插件 API 中相当多的事件结构体存在、WIT 枚举里也列了，
但服务端从未在其对应位置触发它们，插件注册后永不回调。本文把所有这类事件单列为
"陷阱项"。

核查基准：

| 项 | 值 |
|---|---|
| 插件 API | `pumpkin-plugin-api = 0.1.0-dev+26.2-26.45` |
| WIT | `pumpkin-wit/v0.1` |
| 服务端源码 | Pumpkin `0.1.0-dev+26.2-26.45`（`crates/`） |
| 本插件 | `confluxmap-pumpkin` v0.1.1 |

行号均指上述两份资料中的位置。

## 一、宿主能力矩阵

| 能力 | WIT | 服务端实现 | 权限 | 结论 |
|---|---|---|---|---|
| 向指定玩家发自定义载荷 | `player.wit:1353` | `wasm_host/.../player.rs:3764` → `entity/player.rs:81` | 无 | 可用，仅 Java 玩家，任意字节，无长度上限与通道校验；已用于路径点下行帧与 `SERVER_INSTANCE` |
| 接收玩家自定义载荷 | `event.wit:131`（记录）、`:1899`（枚举）、`:2175`（变体） | `net/java/mod.rs:1197-1204`（`fire_blocking` 在 `:1204`） | 无 | 可用，入站上限 32767 字节（`pumpkin-protocol/.../server/play/custom_payload.rs:8`），不可取消；已用于 `confluxmap:waypoints_v1` 上行帧 |
| 通道协商（"客户端是否声明了某通道"） | `event.wit:1005` / `1224` / `1243` | `net/java/mod.rs:1207-1240` | 无 | 可用，但**无查询接口**：只能自己监听 register / unregister 维护集合。`cancelled` 字段无实效（派发处不检查） |
| 在线玩家枚举 | `server.wit:175` | `wasm_host/.../server.rs:116-167` | 无 | 可用，含 UUID 与名字；已用于 `UPSERT` / `REMOVE` 广播 |
| 加入 / 退出 / 登录事件 | `event.wit:35-49` | `world/mod.rs:3327`（加入）、`:5153-5155`（退出）；`server/mod.rs:681`（登录） | 无 | 可用，非陷阱项；退出事件已用于清理路径点会话与订阅 |
| 定时与循环任务 | `scheduler.wit:9,18,23` | `wasm_host/.../scheduler.rs:7,28,50`，每 tick 驱动 `server/mod.rs:1113` | 无 | 可用；无"下一 tick"专用接口，用 `delay = 0` 代替。插件以 20 tick 周期任务刷新 op 变化（`src/lib.rs:343`） |
| 权限 / op 查询 | `permission.wit:11-17`、`player.wit:866` | `wasm_host/.../player.rs:1435-1441`、`server.rs:741`（`is_op`） | 无 | 可用，但 **`permission-level` 枚举在 Rust 侧不可命名**，只能 `as u8` 取判别值（见 §一附录） |
| 命令注册 | `command.wit:17-24` | `wit/v0_1/commands/mod.rs:295,310,591,596` | 无 | 可用，支持子命令；无可选参数标志 |
| 本地持久化 | 无 `fs.wit`，走 WASI preopen | `wasm_host/mod.rs:376-390` → 映射到 `plugins/data/<插件名>` | `fs.read.data` / `fs.write.data` | 可用，**仅限插件私有目录**，无目录外读写；已用于 `shared_waypoints.json` 与 `server_instance.json` |
| UUID 生成 | `uuid.wit:9`（`generate`） | `wasm_host/wit/v0_1/uuid.rs:5-10` | 无 | **插件侧不可达**：`plugin.wit` 的导入列表（`:17-79`）没有 `import uuid`，Rust 绑定不生成该导入（见 §一附录） |
| TCP 监听 / 网络服务 | 无 WIT 接口 | 宿主链接 WASI sockets 与 http：`wasm_host/mod.rs:266-269`；socket 策略 `:334-353` | `network.tcp.bind`、`network.outbound`、`http.outbound` 等 | 可用但需自行基于 `wasi:sockets` 实现协议；WASI 只提供**出站** HTTP，无 HTTP 服务端接口 |
| 已加载区块读写 | `world.wit:800-801`，`chunk` 资源 `:642` | `wasm_host/.../world.rs:655-676`；方块 `world/mod.rs:6052` | 无 | 部分可用：`get-chunk` 未加载即返回 `None`，**无法枚举**已加载区块 |
| 世界存档（region `.mca`） | 无 | 无 | — | 不可用，且插件拿不到世界目录路径 |
| 世界种子 | 无访问器 | — | — | 不可用（本插件因此改为从自己的配置文件读） |
| 世界高度上下限 | 无访问器 | — | — | 不可用，插件拿不到维度的 `min_y` / `max_y`；路径点高度校验因此改用客户端坐标边界（见 §三） |
| 世界 ID / 维度列表 / 服务端版本 / 视距 / 玩家位置 | `world.wit`、`server.wit:245`、`player.rs:1389` | 有 | 视距查询需 `sys.info` 视用途 | 可用；世界 ID 是**世界名**（`wasm_host/.../world.rs:616`），不是 confluxmap 协议的 worldId |
| 超平坦预设 / 生成器 preset | `world.wit:937-938` 仅"设置自定义生成器" | — | — | 读不到世界 preset，不可用 |

### 陷阱项（WIT 有事件声明，服务端无派发点）

插件注册后永不触发，必须按"不可用"处理。与本文相关的关键几项：

| 事件 | 实际情况 |
|---|---|
| `ChunkLoadEvent` | 无构造点，无派发点 |
| `ChunkSaveEvent` / `ChunkSendEvent` | 无派发点 |
| `ChunkUnloadEvent` / `ChunkPopulateEvent` | 构造与 `fire_blocking` 都在 `world/mod.rs:7266-7287`，但这两个方法（`World::unload_chunk` / `World::populate_chunk`）在服务端内无调用者 |
| 其余（`AsyncPlayerChatEvent`、`EntityKnockbackEvent`、`BlockFadeEvent`、`EntityPortalEnterEvent` 等数十项） | 仅结构体与 WIT 枚举存在，服务端无触发点 |

`BlockBreakEvent`（`world/mod.rs:5518`）与 `BlockPlaceEvent`（`block/registry.rs:758`）
是**有真实派发**的，可用来观察玩家造成的方块变更；但它们只覆盖玩家行为，覆盖不了
爆炸、活塞、流体、红石等其它变更来源。

### 附：WIT 有声明，但插件侧够不到

上表是"事件永不触发"这一类；另有一类是接口**声明与宿主实现都在**，但插件世界没有
导入它，Rust 绑定因此不生成，插件代码里根本写不出来。`plugin.wit` 的导入列表
（`:17-79`）是判断依据——既没有 `import uuid`，也没有 `import permission`。

| 声明 | 为什么够不到 |
|---|---|
| `uuid.generate`（`uuid.wit:9`，宿主实现 `wasm_host/wit/v0_1/uuid.rs:5-10`） | 世界未导入 `uuid`，绑定只生成 `Uuid` 记录类型（插件 API 里以 `uuid::Uuid` 出现），`generate` / `parse` / `to_string` 三个函数都没有绑定。插件要 UUID 只能自己填 `high` / `low` |
| `permission-level` 枚举（`permission.wit:11-17`，宿主在 `player.rs:1435-1441` 返回） | 世界未导入 `permission`，`player.get-permission-level`（`player.wit:866`）的返回类型在插件侧**无法命名**，只能对返回值 `as u8` 取判别值（0–4），不能 `match` 具名变体 |

## 二、逐功能可行性

上游 confluxmap 服务端的完整功能面，按可行性分四档：已实现、具备而未实现、暂不实现、
不可行。

### A. 已实现

握手应答、种子下发、worldgen 版本、世界 ID、维度列表、种子共享开关、限流预算声明、
关闭权威纠错、通道声明（`minecraft:register`）、运维命令与配置热重载。见 `README.md`。

v0.1.1 新增：

- **公共路径点**（通道 `confluxmap:waypoints_v1`，协议 1.3）——见 §三；
- **子世界标识 `SERVER_INSTANCE`**（在 `confluxmap:map_sync` 上做能力协商后下发）——见 §四。

### B. 宿主能力具备，本插件尚未实现

这些**做得出来**，只是不在当前范围内的最小形态里。每项后面是"若要做还需什么"。

| 功能 | 依赖的宿主能力 | 备注 |
|---|---|---|
| 玩家位置广播（实体雷达） | 每 tick 任务 + 读取玩家位置 | 具备；需自建限流与开关 |
| 视距下发 `SERVER_VIEW_DISTANCE` | 视距查询 | 具备 |
| 策略热更 `POLICY_UPDATE` | 无（纯协议） | 具备；需先有会话状态跟踪 |
| 能力与兼容性协商 `MAP_COMPATIBILITY` | 无（纯协议） | 具备；当前不下发，走客户端默认兜底 |
| 限流令牌桶、畸形包 strike / 静音、幂等结果缓存 | 无（纯逻辑） | 具备 |
| 结构化错误 `ERROR` | 无（纯协议） | 具备 |
| 命令管理面 `status` / `enable` / `disable` | 命令注册 + 权限查询 | 具备 |
| 更新检查 | `http.outbound` | 具备，但需要网络与隐私考量 |
| 区块加载状态 | 枚举已加载区块 | **不可行**，见 D |
| 权威地图纠错、区域页纠错、纠错失效广播 | 世界存档 | **不可行**，见 D |

### C. 暂不实现（有能力，但判断上不做）

| 功能 | 判断 |
|---|---|
| 网页地图 | 暂不实现，理由见下 |

#### 网页地图为什么暂不实现

上游网页地图是一个同端口的 HTTP + WebSocket 服务（NanoHTTPD / NanoWSD），提供清单、
瓦片与玩家、航点数据，默认只监听 `127.0.0.1`。

宿主能力方面其实没有硬阻塞：宿主链接了 WASI sockets 与 `wasi:http`
（`wasm_host/mod.rs:266-269`），也提供了 `network.tcp.bind` 等权限
（`plugin/permissions.rs`），**监听端口是允许的**；WIT 没有 HTTP 服务端接口、
`wasi:http` 只覆盖出站请求，插件得自己基于 `wasi:sockets` 实现 HTTP 解析、WebSocket
帧、MIME、ETag、CSP 等，且长驻 `accept` 循环与服务端 tick 驱动的配合还需额外验证——
这些只是工作量。

真正的否决理由是**瓦片数据源**：网页地图的价值在于反映服务器**当前**实况，而权威
地图在 Pumpkin 上不可得（见 D）——插件读不到世界存档 region 文件，也拿不到未加载
区块。剩下的唯一通路是"浏览器端用种子自己预测"，也就是纯预测地图；而纯预测地图
不给纠正就没有意义，地图与现实的分歧正是它要解决的问题本身。因此**不做**：等权威
地图通路出现（例如宿主暴露区块读取/枚举能力，或另有可获取权威数据的途径）再重新
评估，在那之前任何形式的网页地图都不提供。

### D. 宿主能力缺失，不可行

| 功能 | 缺失的能力 |
|---|---|
| 权威地图纠错 `MAP_PATCH`、区域页纠错 `MAP_REGION_PATCH` | 读不到世界存档 region 文件，也拿不到未加载区块；无法生成权威地图 |
| 纠错失效广播 | 同上，且缺少覆盖全部变更来源的区块脏标记事件 |
| 区块加载状态 | 无枚举已加载区块的接口，且区块加载 / 卸载事件无派发点 |
| 超平坦基线 `FLAT_BASELINE` | 读不到世界生成器 preset |
| 种子自动获取 | 插件 API 无种子访问器；沙箱只开放插件私有目录 |

## 三、公共路径点专评

**结论：已实现（v0.1.1）。** 它是唯一一项"上游较重、但依赖的宿主能力恰好全部具备"
的功能，现已在测试虚拟机上实测跑通。

实测记录（通道 `confluxmap:waypoints_v1`，协议 1.3）：

| 步骤 | 观测 |
|---|---|
| `HELLO` | 回 `STATUS`，`enabled=1`、`revision=0`、`maxWorld=512`、`maxPlayer=64` |
| `SUBSCRIBE` | 回 `SNAPSHOT`，`count=0` |
| `CREATE` | 回 `RESULT(status=0, error=0)`，并向订阅者广播 `UPSERT(revision=1)` |
| `DELETE` | 回 `RESULT(0,0)`，并广播 `REMOVE(revision=2)` |
| 落盘 | 文件路径正确、内容可读 |
| 同坐标重复 `CREATE` | 回 `status=1, error=11`（`DUPLICATE_LOCATION`） |

上游实现（`common/.../server/shared/`）需要的宿主能力，与 Pumpkin 现状逐条对照：

| 上游所需 | Pumpkin 现状 |
|---|---|
| 独立通道 `confluxmap:waypoints_v1`，帧为该通道上的自定义载荷 | 自定义载荷收发均可用；通道声明已由现有代码用 `minecraft:register` 实现（`src/channel.rs`） |
| 会话跟踪：谁兼容、谁已订阅 | 由 C2S `HELLO` / `SUBSCRIBE` 驱动，纯插件内状态；退出用 `PlayerLeaveEvent`（已确认有派发点）清理 |
| 向单个玩家发送 `RESULT` / `STATUS` / `SNAPSHOT` | `send-custom-payload` 可用 |
| 向所有**订阅者**广播 `UPSERT` / `REMOVE` | 枚举在线玩家 + 逐人发送，可用；是否可发用自行维护的通道注册集合判断（等价于上游 `canSend`） |
| 变更的权限判定：op 或创建者 | op 查询可用（`player.rs:1435-1441` / `server.rs:741`），`allowNonOperatorSharedWaypointManagement` 语义可直接映射；op 变化用 20 tick 周期任务刷新 |
| 每世界 / 每玩家配额、每分变更令牌桶、畸形包 3 次 strike 静音、`operationId` 幂等缓存 | 纯逻辑，没有宿主依赖 |
| 每世界 / 每玩家配额上限、版本乐观锁（`expectedRevision` → `REVISION_CONFLICT`） | 纯逻辑 |
| 持久化：JSON、原子写、损坏文件隔离 | 私有目录可读写（已实测）；**路径不同**，见下方偏差 1 |
| 运维命令 `waypoints status\|enable\|disable` | 命令注册可用；上游 `list [page]` 的可选参数在 Pumpkin 侧需要拆成两个分支（无可选参数标志） |

### 与上游的两处偏差

两处都是宿主能力缺口逼出来的，**不可消除**，需要记在兼容性账上：

1. **持久化位置**。上游写 `<worldRoot>/confluxmap/shared_waypoints.json`（世界存档内），
   本插件只能写 `plugins/data/confluxmap-pumpkin/shared_waypoints.json`：WASI 沙箱只
   preopen 插件私有数据目录（`wasm_host/mod.rs:376-390`），世界根目录对插件不可见。
   **文档形状与上游一致**（schema 2、字段名不变），因此 Paper 端写出来的文件本插件
   仍能读，反之亦然，手工搬运即可互认。另外 Pumpkin 只有一个世界存档，所以是单目录，
   不像上游那样每世界一个目录。

2. **高度校验**。上游按维度的世界高度区间校验 `y`；Pumpkin 没有向插件暴露世界高度
   上下限的接口（见 §一矩阵），因此改用客户端同一套坐标边界：`|x|`、`|z|` ≤
   29_999_984，三轴 `|coord|` ≤ 30_000_000。后果是允许出现"该维度放不下"的 `y` 值，
   只在客户端侧被拦，不在这里拦。

### 其余差异与成本

都不构成阻塞：

1. **单目录**：只有一个世界，无需在文件内以世界名分区。
2. **`LOCK`（0x06）无需实现**：上游服务端当前把它一律拒绝为 `INVALID_REQUEST`
   （`SharedWaypointSessionHandler.java:222-231`），协议里保留但无功能。
3. **可选参数**：`waypoints list [page]` 需拆成 `list` 与 `list <page>` 两条路径。
4. **工作量集中在协议编解码与状态机**：`SharedWaypointCodec` 约 24 KB Java、11 种消息、
   带 minor 版本降级；这是主要成本，而非宿主能力。

## 四、子世界标识（`SERVER_INSTANCE`）专评

**结论：已实现（v0.1.1）。** 依赖的宿主能力只有私有目录持久化一项，其余全是协议。

流程：在 `confluxmap:map_sync` 上做能力协商——解析 `HELLO` 的 `predictorVersion` 里
`|caps2:<base64url>` 报价（协商信封版本 2），回 `0x12 MAP_CAPABILITIES` 选择帧，
**只授予 `SERVER_INSTANCE`（能力 id 7）**，再发 `0x13 SERVER_INSTANCE`（持久化的规范
UUID），最后发 `0x02 HELLO_POLICY`。实测帧：

- `0x12` 共 17 字节：`12020005302e312e310000020202010701`；
- `0x13` 共 39 字节，UUID 与 `server_instance.json` 中的一致；
- 不带 `caps2` 的旧客户端只收到 `0x02`，**无回归**。

持久化在 `plugins/data/confluxmap-pumpkin/server_instance.json`，形如 `{"uuid":"..."}`，
与上游 `UuidFileStore` 同形。

**随机源是个妥协**：插件 API 没暴露随机接口，WIT 的 `uuid.generate` 又因 `plugin.wit`
未导入而在 Rust 侧不可达（见 §一附录），所以改用 `std` 的 `RandomState` 进程熵取两个
`u64` 拼成 UUID（`src/identity.rs:108`）。它只用于命名空间，不是密码学用途，够用；
一旦宿主把 `uuid` 导入插件世界，应换回 `uuid.generate`。

**为什么需要它**：`worldId` 由种子派生，复制世界时会跟着走；Velocity 等代理后面多个
子世界共用同一个 `worldId`，光靠 `worldId` 分不开客户端缓存命名空间——实例 id 就是
用来区分的。
