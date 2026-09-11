# confluxmap 线协议（本插件实现的部分）

来源：`confluxmap/common/src/main/java/cn/net/rms/confluxmap/core/net/`
的 `Proto.java` / `MsgCodec.java` / `HelloC2S.java` / `HelloPolicyS2C.java`。
本插件实现 `HELLO` → `HELLO_POLICY` 这条握手，并在客户端声明能力报价时于策略帧之前
插入 `0x12 MAP_CAPABILITIES` 与 `0x13 SERVER_INSTANCE` 两帧；其余消息码仅作保留说明。
对应插件版本 v0.1.1。

## 通则

- 传输：Minecraft 插件消息（custom payload），通道 `confluxmap:map_sync`。
- 字节序：所有多字节整数 **大端**（big-endian）。
- 字符串：`u16` 字节长度 + 原始 UTF-8 字节（`writeUtf`/`readUtf`），长度上限
  `MAX_UTF8_BYTES = 256`（即长度字段 >256 直接判非法）。
- **无加密**：协议层不含任何对称/非对称加密、MAC 或签名，帧内亦无校验字段。
  confluxmap 中的 SHA-256 与 Deflater 均用于协议之外（客户端缓存标识、区域文件
  与 PNG 编码等）。
- 整帧校验：`MsgCodec.decode` 要求**恰好消费完整个 payload**；多一个尾字节即
  解码失败。本插件的解码器保留同样的严格性。

## `0x01` C2S `HELLO`

玩家进世界后，装了 confluxmap 的客户端在 `confluxmap:map_sync` 通道上发送。
未装 mod 的客户端不会注册该通道，因此**不会**有这条消息 —— 这就是"只发给装了
mod 的玩家"的天然门控。

| 偏移 | 类型 | 字段 | 说明 |
|---|---|---|---|
| 0 | `u8` | `type` | 固定 `0x01` |
| 1 | `utf` | `modVersion` | 客户端 mod 版本，如 `0.2.0` |
| … | `utf` | `predictorVersion` | 预测管线标识（cubiomes commit + shim ABI + 基线算法） |

样例（28 字节）：

```
010005 302e322e30 0012 70726f62652d707265646963746f722d7630
│  │    └ "0.2.0"  │    └ "probe-predictor-v0"
│  └ len=5        └ len=18
└ 0x01 HELLO
```

`predictorVersion` 只用于参考服务端判断能否做残差纠错；**只发种子的服务端可以
忽略，但应当记录** —— 客户端预测地图与实际地形不符时，它是第一个要核对的东西。

自协议主版本 4 起，`predictorVersion` 尾部可带一个能力报价：以 `|caps2:` 引入的
base64url 数据块（信封版本 2，含全部 8 个能力）。插件只从中判断客户端是否提供
`SERVER_INSTANCE`（能力 id 7，版本 1），其余能力一律不授予——它们都属于纠错流。
未带该标记的旧客户端按上一版处理，只收到 `0x02 HELLO_POLICY`。帧序见
「握手帧序与能力协商」。

## `0x02` S2C `HELLO_POLICY`

服务端收到 HELLO 后的应答。本插件构造的帧字段顺序：

| # | 类型 | 字段 | 本插件取值 |
|---|---|---|---|
| 0 | `u8` | `type` | `0x02` |
| 1 | `u8` | `flags` | `SEED_GRANTED`(bit0)=1，其余 0 |
| 2 | `utf` | `worldId` | 由种子派生，见下 |
| 3 | `utf` | `worldgenVersion` | 服务端 MC 版本串，如 `26.2` |
| 4 | `i32` | `budgets.maxBytesPerSec` | `262144` |
| 5 | `u16` | `budgets.maxTilesPerReq` | `8` |
| 6 | `u16` | `budgets.minReqIntervalMs` | `100` |
| 7 | `u8` | `budgets.maxPatchLod` | `4` |
| 8 | `u8` | `dimCount` | 维度条目数（≤ `MAX_DIM_ENTRIES = 8`） |
| 9+ | ×N | `DimDescriptor` | 见下 |

每个 `DimDescriptor`：

| 类型 | 字段 |
|---|---|
| `utf` | `id`（如 `minecraft:overworld`） |
| `utf` | `kind`（`overworld` / `the_nether` / `the_end`） |
| `u8` | `dimBits` |
| `i64` | `seed`（大端） |

`dimBits` 位布局：

| 位 | 含义 |
|---|---|
| 0 | `predictable` —— 该维度可预测 |
| 1 | `hasSeed` —— 下面的 `seed` 有效 |
| 2..=4 | `preset`（`WorldPreset.wireId()`，0 = `DEFAULT`） |
| 5..=7 | 保留 |

维度在列表中的**下标即 `dimIndex`**，客户端后续请求会回显它，因此顺序是协议契约的
一部分。`preset` 位在旧客户端会被掩掉、按 `DEFAULT` 读，是向后兼容设计。

### `flags` 位表

| 位 | 名称 | 本插件 |
|---|---|---|
| 0 | `seedGranted` | **总是置 1**（有种子且允许共享时） |
| 1 | `correctionsEnabled` | **恒 0** —— 这是"只发种子、不纠错"的定义 |
| 2 | `biomeMapForbidden` | 0 |
| 3 | `chunkLoadStateEnabled` | 0 |
| 4 | `entityRadarForbidden` | 0 |
| 5 | `correctionInvalidationEnabled` | 0 |
| 6 | `chunkRangeCorrectionEnabled` | 0 |
| 7 | `structureSearchForbidden` | 0 |

### 客户端如何解释

`correctionsEnabled = 0` → 客户端进入 `ClientMode.SERVER_DISABLED`：
会话保持 **ACTIVE**、种子可用、客户端自己用种子生成预测地图，但**从不**请求
权威补丁。这正是本插件的形态，且客户端原生支持，无需改客户端。

这条路在客户端代码里是可逐行验证的，不依赖"大概能用"：

1. `CompanionSession.onPolicy` 先调 `MapSyncProtocol.acceptServer(pendingSelection, policy, ...)`。
   `pendingSelection` 只在服务端发过 `MapSyncCompatibilityS2C` / `MapCapabilitiesS2C` 时才非空；
   未带 caps2 报价的客户端两者都收不到，`pendingSelection` 为空，于是走到 `acceptServer`
   最后一段兜底返回：
   ```java
   !policy.flags().correctionsEnabled()
       ? NegotiatedMapSync.CorrectionMode.DISABLED
       : ...
   ```
2. 回到 `selectMapSyncMode`：`DISABLED` 分支里
   `!received.flags().correctionsEnabled() && !selectionDisablesCorrections()`
   → `true && true` → 返回 `ClientMode.SERVER_DISABLED`（不是 `INCOMPATIBLE`）。
3. `onPolicy` 随后 `state.set(State.ACTIVE)`，`policy` 原样保留（不会走 `withoutCorrections`）。

声明了 caps2 的客户端先收到 `0x12`，其选择帧自身的 `correctionMode` 就是 `DISABLED`
（reason `NO_COMMON_WIRE`）。两条路径的结论一致：客户端都停在 `SERVER_DISABLED`，
不会进入任何纠错流程。

所以握手是**一轮**：旧客户端收一帧 `HELLO_POLICY`；带 caps2 报价的客户端在策略帧
之前先收 `0x12`（必要时还有 `0x13`）。额外的帧是服务端到客户端的单向前置声明，
不产生往返，因此不存在"协商多打一轮"的问题。客户端日志会打：

```
companion active (worldId=... worldgen=26.2 seedGranted=true corrections=false
                  mapSyncMode=SERVER_DISABLED biomeMapAllowed=true
                  structureSearchAllowed=true chunkLoadState=false entityRadarAllowed=true)
```

这一行是客户端侧"成功"的唯一权威判据。

### 与 Paper 参考实现的 flag 等价性

把官方 `CompanionPolicy.configuredFlags` 按 `enabled=true, shareSeed=true,
shareCorrections=false` 代入，得到：

| 位 | Paper（上述配置） | 本插件 |
|---|---|---|
| `seedGranted` | 1 | 1 |
| `correctionsEnabled` | 0 | 0 |
| `biomeMapForbidden` | 0 | 0 |
| `chunkLoadStateEnabled` | 0 | 0 |
| `entityRadarForbidden` | 0 | 0 |
| `correctionInvalidationEnabled` | 0 | 0 |
| `chunkRangeCorrectionEnabled` | 0 | 0 |
| `structureSearchForbidden` | 0 | 0 |

即 flag 字节 `0x01` 与官方 Paper 伙伴"关掉纠错"时**完全一致**。本插件不是自创
一种形态，而是复刻了参考实现的一个既有配置点。

### 客户端侧的 worldgen 版本白名单

客户端用 `McVersions.toCubiomes(worldgenVersion)` 把下发串映射到 cubiomes 的
`MCVersion` 常量；映射为空即视为不支持的版本。本插件下发的是**服务端 MC 版本串**，
取值 `"26.2"` 命中白名单（→ `MCVersion 31`，即 "Chaos Cubed" 世界生成族）。
白名单覆盖 1.7 至 26.2 的每个真实发布串，且对更新的补丁版按 minor 线回退到该线最新常量
（`NEWEST_BY_LINE`），因此**服务端升级补丁版不会让预测失效**，最坏情况是用旧参数预测。
注意 26.1 与 26.2 是**不同的** cubiomes 常量（30 vs 31）：26.2 新增了 Chaos Cubed 族，
把 26.2 的世界当成 26.1 预测会错。

## 黄金向量（跨实现校验）

`tools/PolicyVector.java` 用 confluxmap 的**真实** `MsgCodec.encode` 生成一帧
`HELLO_POLICY`，作为 Rust 实现的权威对照。相同输入：

- seed = `81985529216486895`（`0x0123456789ABCDEF`，**合成值**）
- worldId = `00000000-0000-0000-0000-456789abcdef`
- worldgenVersion = `26.2`
- dims = `[minecraft:overworld / overworld / predictable+hasSeed]`
- budgets = 默认值

产出 **97 字节**：

```
0201002430303030303030302d303030302d303030302d303030302d343536373839616263646566
000432362e320004000000080064040100136d696e6563726166743a6f766572776f726c6400096f
766572776f726c64030123456789abcdef
```

该向量已固化进 `src/protocol.rs` 的 `encoder_matches_the_reference_implementation_byte_for_byte`
测试；`cargo test` 通过即表示 Rust 编码器与参考实现逐字节一致。

> **关于向量的来源**：参考编码器最初是对着一个真实服务器的种子跑的，而真实种子不
> 应进入公开仓库。因此向量改用上面的合成种子，且它是从那次参考输出**推导**而来而
> 非重新生成——只替换了两个由种子派生的字段（`worldId` 末尾 12 个十六进制字符、
> 末尾 8 字节 `seed`），其余每个字节都是参考编码器自己的输出。
>
> 这个替换是**精确等价**而非近似，因为 `encode` 是逐字段直写：`flags`、
> `worldgenVersion`、`budgets`、各维度描述符都与种子无关，`worldId` 是以等长
> （36 字节）的 `utf` 原样写入的不透明串，维度种子是定宽大端 `i64`。所以上表输入
> 经参考编码器产出的就是这一帧。**期望字节仍源自 Java 实现，不是 Rust 实现的自我
> 循环验证。**

## 主通道消息清单

`Proto` 定义的 `confluxmap:map_sync` 消息码全表，以及本插件的处理方式：

| 码 | 方向 | 消息 | 本插件 |
|---|---|---|---|
| `0x01` | C2S | `HELLO` | 解析并应答 |
| `0x02` | S2C | `HELLO_POLICY` | 发送 |
| `0x03` | C2S | `MAP_VIEW_REQ` | 忽略 |
| `0x04` | S2C | `MAP_PATCH` | 不发送 |
| `0x05` | S2C | `POLICY_UPDATE` | 不发送 |
| `0x06` | S2C | `ERROR` | 不发送 |
| `0x07` | S2C | `FLAT_BASELINE` | 不发送 |
| `0x08` | C2S | `LOAD_STATE_SUBSCRIBE` | 忽略 |
| `0x09` | S2C | `LOAD_STATE_DELTA` | 不发送 |
| `0x0A` | C2S | `MAP_SYNC_SUBSCRIBE` | 忽略 |
| `0x0B` | S2C | `MAP_INVALIDATE` | 不发送 |
| `0x0C` | C2S | `MAP_REGION_VIEW_REQ` | 忽略 |
| `0x0D` | S2C | `MAP_REGION_PATCH` | 不发送 |
| `0x0E` | C2S | `MAP_REGION_SYNC_SUBSCRIBE` | 忽略 |
| `0x0F` | S2C | `MAP_REGION_INVALIDATE` | 不发送 |
| `0x10` | S2C | `MAP_COMPATIBILITY` | 不发送 |
| `0x11` | S2C | `SERVER_VIEW_DISTANCE` | 不发送 |
| `0x12` | S2C | `MAP_CAPABILITIES` | 对带 caps2 报价的客户端发送；能力列表为空或仅 `SERVER_INSTANCE` |
| `0x13` | S2C | `SERVER_INSTANCE` | 仅在 `0x12` 授予了 `SERVER_INSTANCE` 时发送，携带本实例 UUID 的规范字符串 |
| `0x14` | S2C | `PLAYER_POSITIONS` | 不发送 |

「忽略」表示该码到达同一通道但并非 `HELLO`：插件记录一条日志后不作处理。各 S2C 帧
之所以其余 S2C 帧一律不发，是因为策略里对应的开关与能力位全部为 0（如
`correctionsEnabled = 0`），客户端不会进入相关流程。哪些帧将来能发、哪些根本发不了，见
[pumpkin-capabilities.md](pumpkin-capabilities.md)。

## 握手帧序与能力协商

带 caps2 报价的客户端，一次 HELLO 的回复帧序为：

1. `0x12 MAP_CAPABILITIES`（能力选择帧）
2. `0x13 SERVER_INSTANCE`（仅当授予了 `SERVER_INSTANCE`）
3. `0x02 HELLO_POLICY`（总是最后一帧）

**顺序不可换。** `SERVER_INSTANCE` 是能力门控消息：客户端从 `0x12` 解析出被选中的
能力，未在 `0x12` 中选中的能力，`0x13` 会被直接拒绝（`NegotiatedMapSync.requireCapability`）。
`HELLO_POLICY` 必须最后发，因为客户端在策略帧上打开会话。

未带 caps2 标记的旧客户端只收到 `0x02`，与上一版完全一致；插件不会因升级而改变
它们的行为。

### 能力报价的解析

`predictorVersion` 尾部的 `|caps2:<base64url>` 数据块（信封版本 2）经 base64url 解码后
包含客户端提供的全部 8 个能力。插件**只授予 `SERVER_INSTANCE`（能力 id 7，版本 1）**：
其余能力都属于纠错流，本插件不实现，授予会让客户端等待永不到达的消息。因此能力列表
要么为空，要么只有一项。

### `0x12 MAP_CAPABILITIES` 帧格式

| # | 类型 | 字段 | 本插件取值 |
|---|---|---|---|
| 0 | `u8` | `type` | `0x12` |
| 1 | `u8` | negotiation version | `2` |
| 2 | `utf` | 服务端 mod 版本 | 插件自身版本号 |
| 3 | `utf` | 基线预测器标识 | 空串（本插件不声明基线） |
| 4 | `u8` | `correctionMode` | `2` = `DISABLED` |
| 5 | `u8` | `reason` | `2` = `NO_COMMON_WIRE` |
| 6 | `u8` | `correctionProfile` | `2` = `SOURCE_LIGHT_V2`（仅取形状，纠错关闭时不解码纠错体） |
| 7 | `u8` | 能力数 | `0` 或 `1` |
| 8+ | ×N | 能力项 | `u8` id + `u8` version，本插件只有 `7` / `1` |

### `0x13 SERVER_INSTANCE` 帧格式

| # | 类型 | 字段 | 本插件取值 |
|---|---|---|---|
| 0 | `u8` | `type` | `0x13` |
| 1 | `utf` | `instanceId` | 本实例的 UUID 规范字符串 |

实例 id 首次使用时生成并持久化于 `plugins/data/confluxmap-pumpkin/server_instance.json`
（`{"uuid": "..."}`，与上游 `UuidFileStore` 同形）；文件不可读时重新生成并告警。它与
`worldId` 是两件事：`worldId` 存在世界存档里，会随被复制的世界一起走；实例 id 存在
插件配置旁，不会。Velocity 等代理后面多个子世界共用同一个 `worldId` 时，客户端靠实例
id 区分存储命名空间，避免地图数据互相覆盖。

## 公共路径点通道（`confluxmap:waypoints_v1`）

共享航点走独立通道 `confluxmap:waypoints_v1`，协议 1.3（上游 `SharedWaypointProto`）。

| 码 | 方向 | 消息 | 本插件 |
|---|---|---|---|
| `0x01` | C2S | `HELLO` | 处理：以 `0x02 STATUS` 应答 |
| `0x02` | S2C | `STATUS` | 发送 |
| `0x03` | C2S | `SUBSCRIBE` | 处理：回一条 `0x07 SNAPSHOT` |
| `0x04` | C2S | `CREATE` | 处理 |
| `0x05` | C2S | `DELETE` | 处理 |
| `0x06` | C2S | `LOCK` | 一律回 `RESULT` / `INVALID_REQUEST` |
| `0x07` | S2C | `SNAPSHOT` | 发送 |
| `0x08` | S2C | `UPSERT` | 发送（增量广播） |
| `0x09` | S2C | `REMOVE` | 发送（增量广播） |
| `0x0A` | S2C | `RESULT` | 发送 |
| `0x0B` | C2S | `UPDATE` | 处理 |

语义照上游：全局 revision 单调递增、每次变更 +1；`expectedRevision` 乐观并发；
`operationId` 幂等，重连重试不会重复发布；配额、权限与限流（控制请求 8 突发 /
每分钟 60；变更 10 突发、每分钟数由 `waypoint_mutations_per_minute` 配置）；畸形包
累计 3 次后静音到断线。`LOCK` 一律返回 `INVALID_REQUEST`——服务端标记已随上游移除，
该消息码只为兼容而保留。

权限：op 等级 ≥ 2 视为管理员；`allow_non_operator_waypoint_management`（默认 `true`）
允许非 op 管理自己发布的点。

持久化在 `plugins/data/confluxmap-pumpkin/shared_waypoints.json`，schema 2，文档形状与
上游一致（`schemaVersion` / `revision` / `ownerInstanceId` / `waypoints[]`），只有位置
不同——WASI 沙箱只开放插件私有数据目录，插件读不到世界存档目录。损坏的文档隔离为
`.bad` 后重建；schema 更高的文档保留不动且功能置为不可用；带着别的服务端
`ownerInstanceId` 的文档改名为 `.bak`。

与上游的第二处差异：高度校验用的是客户端同一套坐标边界（|coord| ≤ 3000 万、
水平 ≤ 29999984），而不是按维度取世界高度上下限——Pumpkin 没有把世界高度暴露给
插件的接口。

网页地图不在本插件范围内：上游瓦片来自读存档的纠错服务，此处没有权威地图可供给，
退化为浏览器端按种子预测只是重复客户端已有的本地预测，因此不做。
