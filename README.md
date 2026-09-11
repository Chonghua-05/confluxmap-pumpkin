# confluxmap-pumpkin

[![CI](https://github.com/Chonghua-05/confluxmap-pumpkin/actions/workflows/ci.yml/badge.svg)](https://github.com/Chonghua-05/confluxmap-pumpkin/actions/workflows/ci.yml)

[confluxmap](https://github.com/Chonghua-05/conflux-map) 在
[Pumpkin](https://github.com/Pumpkin-MC/Pumpkin) 服务端上的伴侣插件。

## 上游项目

| 项目 | 说明 |
|---|---|
| **confluxmap** | Minecraft 客户端 mod 及其服务端伴侣（Fabric 服务端入口，以及面向 Paper / Folia 的插件）。它在客户端以世界种子本地生成预测地图，再向服务端请求权威补丁以校正预测偏差。本插件实现其 `confluxmap:map_sync` 通道的握手。 |
| **Pumpkin** | 以 Rust 实现的 Minecraft 服务端。插件以 `wasm32-wasip2` 组件形态加载，运行于 WASI 沙箱内，通过 WIT 接口调用宿主能力，并按插件元数据中声明的权限获得授权。 |

## 功能支持

| 能力 | 状态 | 说明 |
|---|---|---|
| 握手应答 | 已支持 | 在 `confluxmap:map_sync` 上收到 `HELLO` 后应答一帧 `HELLO_POLICY`；握手在单次往返内完成，其后不再有往来消息 |
| 世界种子 | 已支持 | 置 `seedGranted = 1`，逐维度附带种子 |
| worldgen 版本 | 已支持 | 客户端据此选择地形生成参数 |
| 世界 ID | 已支持 | 客户端用作地图缓存的命名空间 |
| 维度列表 | 已支持 | 逐维度给出可预测性与生成器 preset；默认仅 `minecraft:overworld` |
| 权威地图纠错 | 未支持 | `correctionsEnabled = 0`，客户端据此进入 `SERVER_DISABLED`：会话保持 ACTIVE、种子可用，但不请求权威补丁 |
| 区块加载状态 | 未支持 | 对应标志未下发 |

`biomeMapForbidden`、`structureSearchForbidden`、`entityRadarForbidden` 三个限制标志本插件一律不下发，客户端维持其默认行为。

### 未支持部分的原因

confluxmap 的完整形态是"预测 → 校正"。该链条在 Pumpkin 上无法成立，原因均在 Pumpkin 侧：

| 所需能力 | Pumpkin 侧现状 |
|---|---|
| 世界种子 | 插件接口未提供种子访问器；插件运行于 WASI 沙箱，只能访问自身的私有数据目录，读不到 `pumpkin.toml` 与世界存档。种子因此需人工写入插件配置文件（见「配置文件」） |
| 世界存档 | 同上：插件文件系统不包含世界存档，无法读取 region 文件以生成权威地图 |
| 非驻留区块 | 区块读取接口只在已加载的区块中查找，未命中即返回空值，且不会触发区块加载 |
| 区块生命周期事件 | 插件接口声明了区块加载 / 卸载事件，但服务端当前版本没有在区块生命周期上派发它们的调用点 |

因此，本插件实现"以种子生成预测地图"所需的握手；"以存档生成权威地图并下发补丁"在插件接口内没有可行路径，故不予实现，并在策略中明确关闭。

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

安装时需注意两点：

- **权限确认。** Pumpkin 默认 `ask_permission_confirmation = true`，插件首次加载会在控制台列出其申请的权限（`fs.read.data`、`fs.write.data`）并等待确认；控制台不可交互时按拒绝处理，插件不会加载。以无控制台方式启动的服务端，请设置 `[plugins] ask_permission_confirmation = false`，或将该插件申请的权限列入 `allowed_permissions` 预先批准。已作出的决定按插件文件哈希记录于 `plugins/permission_cache.json`，插件文件更新后需重新确认。
- **未签名插件。** 本插件未签名，加载时会打印相应告警。Pumpkin 默认 `allow_unsigned = true`，会继续加载；若服务端已关闭该项，可通过 `[plugins.overrides.confluxmap-pumpkin]` 的 `allow_unsigned = true` 单独放行，或对插件签名。

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

## 配置文件

`plugins/data/confluxmap-pumpkin/config.toml`，首次加载时由插件写出，全部键均以注释形式给出。插件向宿主申请的权限为 `fs.read.data` 与 `fs.write.data`，即只限该私有目录，不申请环境变量、网络或其他权限。

| 键 | 必填 | 说明 |
|---|---|---|
| `seed` | 是 | 服务端世界种子。接受十进制有符号整数，亦接受按 `u64` 位模式书写的形式（用于大于 `i64::MAX` 的显示值），允许 `_` 作为数字分隔符。未填写时插件仍会应答握手，但 `seedGranted = 0`，客户端不显示地图 |
| `share_seed` | 否 | 设为 `false` 时不下发种子（`seedGranted = 0`），插件仍会应答握手；缺省 `true` |
| `worldgen` | 否 | 显式指定 worldgen 版本串；缺省由服务端版本串解析 |
| `world_id` | 否 | 显式指定世界 ID；缺省由种子派生为 `00000000-0000-0000-0000-<种子低 48 位>` |
| `dims` | 否 | 维度 id 列表，可写逗号分隔的字符串或 TOML 数组；缺省仅 `minecraft:overworld`。已知原版维度标记为可预测，其余按不可预测处理 |

解析规则宽松：每行一个 `key = value`，`#` 起注释，值可加引号。无法解析的值与未知的键只会在日志中告警并保留缺省值，不会导致插件加载失败。

## 目录

```
src/protocol.rs  线协议编解码，逐字节镜像 confluxmap 的 MsgCodec.java
src/channel.rs   向客户端声明 confluxmap:map_sync 通道
src/config.rs    插件配置文件（模板、解析与校验）
src/handshake.rs HELLO -> HELLO_POLICY 应答逻辑
src/commands.rs  /cfm 命令树
src/state.rs     配置快照、计数器与通道声明的去重
src/lib.rs       Plugin 入口与事件注册
docs/protocol.md             HELLO / HELLO_POLICY 线格式与客户端判定链路
tools/PolicyVector.java      以参考 Java 编码器生成黄金向量
tools/inject_seed.py         由 pumpkin.toml 幂等写入插件配置
tools/test_inject_seed.py    种子写入脚本的回归测试
build.sh / test.sh           构建与测试
```

## 许可

LGPL-3.0-or-later，见 [LICENSE](LICENSE)。
