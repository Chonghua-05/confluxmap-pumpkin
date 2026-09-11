# confluxmap-pumpkin

[![CI](https://github.com/Chonghua-05/confluxmap-pumpkin/actions/workflows/ci.yml/badge.svg)](https://github.com/Chonghua-05/confluxmap-pumpkin/actions/workflows/ci.yml)

[confluxmap](https://github.com/Chonghua-05/conflux-map) 客户端在
[Pumpkin](https://github.com/Pumpkin-MC/Pumpkin)（南瓜端）上的服务端伴侣插件。

## 上游项目

| 项目 | 是什么 | 与本插件的关系 |
|---|---|---|
| **confluxmap** | 一个 Minecraft 客户端 mod + Paper 服务端伴侣。它用**世界种子**在本地预测地形，再向服务端请求"权威补丁"来校正预测与真实世界的偏差。 | 客户端侧的协议实现者。本插件响应的是它发出的握手。 |
| **Pumpkin** | 用 Rust 写的 Minecraft 服务端，插件以 **WASM**（`wasm32-wasip2`）形态加载，通过 WIT 接口与宿主通信，运行在能力式沙箱里。 | 本插件的运行宿主。 |

## 目前只支持了什么

**只有一件事：握手——把世界种子和 worldgen 版本发给客户端。**

装了 confluxmap 的玩家进世界后，会在 `confluxmap:map_sync` 通道上发一个 `HELLO`。
本插件回一个 `HELLO_POLICY` 帧，里面是：

- 服务器**世界种子**（`seedGranted=1`）
- **worldgen 版本**串（客户端用它选 cubiomes 的生成参数）
- 世界 ID、维度列表
- `correctionsEnabled=0` —— 明确告诉客户端"我不提供纠错"

客户端拿到种子后进入 `SERVER_DISABLED` 模式：预测地图照常生成并使用，但**从不**向
服务端请求权威补丁。一轮握手即完成，没有后续往来。

**没有**纠错、没有区块扫描、没有权威地图、没有内嵌 Web 地图后端。

### 为什么只支持这些：Pumpkin 的限制

confluxmap 的完整形态是"预测 → 校正"，而这条链在 Pumpkin 上**每个环节都被堵住**，
这些是实测确认的硬边界，不是没做：

| 缺失的能力 | Pumpkin 的限制 | 卡住什么 |
|---|---|---|
| 世界种子 | 插件 API **没有种子访问器**；WASI 沙箱又读不到 `pumpkin.toml` 与 `world/` | 连种子都拿不到 → 只能由管理员从外部注入（见下节） |
| 世界存档 | 沙箱只映射插件自己的数据目录；相对上溯、绝对路径、目录枚举、符号链接逃逸**全部 ENOENT** | 无法读 region 文件生成权威地图 |
| 非驻留区块 | 区块 API 只能看到"因玩家出现而驻留"的区块；读非驻留区块返回默认空值，且不会触发加载 | 无法回读存档做校正 |
| 区块生命周期事件 | `ChunkLoadEvent` / `ChunkUnloadEvent` 等**实测从不触发** | 无法事件驱动地扫描区块 |

所以"用种子生成预测地图"这一步还能做（种子注入后即可），而"用存档生成权威地图并下发
补丁"这一步在 WASM 插件里无从下手。本插件取前者，把后者明确关掉。

另外，插件消息（custom payload）本身是 **Java 版专属**：Bedrock 客户端没有这条通道，
所以本插件对 Bedrock 客户端不产生任何效果。

## 适用的 Pumpkin 版本

| 项 | 值 |
|---|---|
| 锁定的插件 API | `pumpkin-plugin-api = 0.1.0-dev+26.2-26.45` |
| 对应服务端 | Pumpkin `0.1.0-dev+26.2-26.45` |
| 对应 Minecraft | 26.2（Java 协议 776） |
| 运行形态 | `wasm32-wasip2` 插件，丢进 `plugins/` 目录即加载，无需改服务端、无需重编译服务端 |

⚠️ **API 版本号与服务端 build 是一一对应的**，不是语义化兼容：Pumpkin 的开发版 API
直接绑定服务端的 WIT ABI，服务端升级后必须同步改这个锁并重新编译，否则插件加载失败。
（`Cargo.lock` 里也锁了具体 build，两者要一起改。）

客户端侧没有这个约束：Pumpkin **不做跨版本门禁**，实测 1.17.1（协议 755）客户端也能
连上 26.2 服务端并完成握手。

## 安装

1. 从 [Releases](https://github.com/Chonghua-05/confluxmap-pumpkin/releases) 下载
   `confluxmap_pumpkin.wasm`（或自行 `./build.sh`），放进服务端的 `plugins/` 目录。
2. 把种子注入给插件 —— Pumpkin 的插件 API 没有种子访问器，WASI 沙箱里也读不到
   `pumpkin.toml`，因此种子通过插件环境变量覆盖传入（见下节）。
3. 重启服务端。

> **首次加载会被问权限**：插件未签名，Pumpkin 默认
> （`ask_permission_confirmation = true`）会**在 stdin 上交互式询问**是否授予权限。
> 没有 TTY 的部署方式会卡在这里，生产环境请先在终端确认一次。

### 种子从哪来

`pumpkin.toml`：

```toml
[plugins.overrides.confluxmap-pumpkin.environment]
CFM_SEED = "<与服务器顶层 seed 一致>"
```

`tools/inject_seed.py` 会幂等地写入这段配置，种子值**直接从 `pumpkin.toml` 顶层
`seed` 字段复制**，不会漂移：

```bash
python tools/inject_seed.py /path/to/pumpkin.toml
```

`CFM_WORLDGEN` 默认不写：插件从运行中服务端的 `pumpkin-version` 字符串自动解析 MC
版本（如 `0.1.0-dev+26.2-26.45` → `26.2`），服务器升级后不会过期。

## 构建与测试

```bash
./build.sh   # cargo build --release -> target/wasm32-wasip2/release/confluxmap_pumpkin.wasm
./test.sh    # 单元测试（含与参考 Java 编码器逐字节对齐的黄金向量测试）

python3 tools/test_inject_seed.py   # 种子注入脚本的回归测试
```

前两者是 Windows/Git Bash 下 re-create MSVC 环境的包装脚本：Git Bash 的 GNU coreutils
`link` 会抢在 MSVC `link.exe` 前面，导致 rustc 链接报 `link: extra operand`。Linux/macOS
或 CI 上直接 `cargo build --release` / `cargo test` 即可。

`test.sh` 用 `--target x86_64-pc-windows-msvc` 在**宿主**上跑测试：协议与配置模块是纯
Rust 且无宿主调用，API crate 生成的绑定也能编到原生 target，所以测试不必进 wasm。

## 运维命令

| 命令 | 说明 |
|---|---|
| `/cfm status` | 配置摘要：种子是否就绪、worldgen 版本、已握手客户端数 |
| `/cfm seed` | 显示当前下发的种子（只读） |
| `/cfm hello` | 回放最近一次收到的 HELLO 的解析结果 |
| `/cfm reload` | 重新读取环境变量配置 |

## 环境变量

| 变量 | 必填 | 说明 |
|---|---|---|
| `CFM_SEED` | 是 | 服务器种子（十进制有符号 i64） |
| `CFM_WORLDGEN` | 否 | 显式指定 worldgen 版本串；缺省从 `pumpkin-version` 解析 |
| `CFM_WORLD_ID` | 否 | 下发给客户端的 worldId；缺省由种子派生 `00000000-0000-0000-0000-<seed低48位>` |
| `CFM_DIMS` | 否 | 逗号分隔的维度名；缺省仅 `minecraft:overworld` |
| `CFM_SHARE_SEED` | 否 | 设为 `false` 时 `seedGranted=0`，客户端只感知服务器存在、拿不到种子（缺省 `true`） |

## 目录

```
src/protocol.rs   线协议编解码（逐字节镜像 confluxmap 的 MsgCodec.java）
src/channel.rs    向客户端声明 confluxmap:map_sync（缺了它客户端不发 HELLO）
src/config.rs     环境变量配置解析
src/handshake.rs  HELLO -> HELLO_POLICY 应答逻辑
src/commands.rs   /cfm 命令树
src/state.rs      配置快照、计数器与补声明去重
src/lib.rs        Plugin 入口与事件注册
docs/protocol.md  HELLO/HELLO_POLICY 线格式说明与客户端判定链路
tools/PolicyVector.java  用参考 Java 编码器生成黄金向量
tools/inject_seed.py     幂等种子注入（不会动到 pumpkin.toml 里其他表）
tools/test_inject_seed.py  注入脚本的回归测试
build.sh / test.sh       构建与测试
```

## 许可

LGPL-3.0-or-later，见 [LICENSE](LICENSE)。
