# confluxmap-pumpkin

[![CI](https://github.com/Chonghua-05/confluxmap-pumpkin/actions/workflows/ci.yml/badge.svg)](https://github.com/Chonghua-05/confluxmap-pumpkin/actions/workflows/ci.yml)

confluxmap 的 [Pumpkin](https://github.com/Pumpkin-MC/Pumpkin)（南瓜端）服务端插件。

玩家进世界后，confluxmap 客户端会在 `confluxmap:map_sync` 通道上发送 `HELLO`（携带自己的
mod 版本）。本插件收到 HELLO 后回一个 `HELLO_POLICY` 帧告知：服务器种子
（`seedGranted=1`）、worldgen 版本、世界 ID、维度列表，同时 `correctionsEnabled=0` ——
客户端进入 `SERVER_DISABLED` 模式：会话保持活跃、用拿到的种子在本地生成预测地图，
但**不请求纠错**。

也就是说，这是个**最小可行版本：只发种子和版本，不做纠错**。它解决的是"多人服务器上
客户端拿不到种子，预测地图没法用"这件事，而不是复刻完整的 Paper 伙伴（那套含权威补丁、
区块轮询、内嵌 Web 地图后端）。

## 为什么这个形态是安全的

- **天然门控**：只有装了 confluxmap 的客户端才会注册该通道并发 HELLO，所以"种子/版本
  只发给装了 mod 的玩家"是事件模型自带的，无需额外判断。
- **不依赖易变 API**：只用 `PlayerCustomPayloadEvent` 与 `player.send_custom_payload`
  这两个最基础的插件面；不做区块轮询、不做纠错，Pumpkin 未来 API 变动的影响面最小。
- **协议零加密**：confluxmap 全协议层无加密（仅 SHA-256 标识哈希 + Deflater 压缩），
  服务端实现不需要任何密码学。

## 安装

1. 从 [Releases](https://github.com/Chonghua-05/confluxmap-pumpkin/releases) 下载
   `confluxmap_pumpkin.wasm`（或自行 `./build.sh`），放进服务端的 `plugins/` 目录。
2. 把种子注入给插件 —— Pumpkin 的插件 API 没有种子访问器，WASI 沙箱里也读不到
   `pumpkin.toml`，因此种子通过插件环境变量覆盖传入（见下节）。
3. 重启服务端。

> **首次加载会被问权限**：插件未签名，Pumpkin 默认
> （`ask_permission_confirmation = true`）会**在 stdin 上交互式询问**是否授予权限。
> 没有 TTY 的部署方式来会卡在这里，生产环境请先在终端确认一次。

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

`CFM_WORLDGEN` 默认不写：插件从运行中服务端的 `pumpkin-version` 字符串自动解析 MC 版本
（如 `0.1.0-dev+26.2-26.45` → `26.2`），服务器升级后不会过期。

## 关键实现细节：必须主动向客户端声明通道

**这是最容易漏掉、且漏掉后完全看不出来的一步。**

confluxmap 客户端在发 HELLO 之前会检查服务端是否**已经向它声明过** `confluxmap:map_sync`
（`ClientPlayNetworking.canSend` → `ClientPacketListener.hasChannel`）。没声明就不发，
而且失败只记在客户端 **debug 级**日志里 —— 服务端侧看起来一切正常，没有任何错误。

在 Paper 上这一步是 Bukkit 替插件做的（注册插件通道时自动下发）。Pumpkin 没有对应 API，
所以插件自己发原版包：`minecraft:register`，载荷是 NUL 分隔的通道名。

时序同样关键：客户端在**自己的 JOIN 回调**里发 HELLO，所以声明必须比它更早。本插件挂在
`PlayerLoginEvent`（实测在登录包之前触发）上，`PlayerJoinEvent` 作为第二次尝试，客户端
注册通道时再补一次（**每会话最多一次** —— 一次进服会注册几十个装载器通道，不设上限就会
变成几十个冗余包和几十行日志）。

```
22:25:44 [confluxmap] announced confluxmap:map_sync to TestClient at login
22:25:44 [confluxmap] re-announced confluxmap:map_sync to TestClient at join
22:25:44 [confluxmap] HELLO #1 TestClient modVersion="0.2.0" -> HELLO_POLICY sent 97B ...
```

### 各加载器的注册行为

客户端**是否**回发旧式 `minecraft:register` 取决于装载器，`/cfm status` 里的
`channels registered` 计数**不能**当作"成功了没有"的判据：

| 客户端 | 旧式 `minecraft:register` | 结论 |
|---|---|---|
| Fabric（1.17.1 / 26.2） | **会发**（含 `confluxmap:map_sync`） | 计数 +1，日志有 `confluxmap client detected` |
| NeoForge 26.1 | **不发**（走 payload 类型注册） | 计数不变，**但 HELLO 照常到达** |

唯一的权威判据是 **`handshakes`**（HELLO 计数）以及客户端的 `mapSyncMode=SERVER_DISABLED`。

⚠️ **测试注意**：手工发包的测试客户端如果直接发 HELLO（不检查声明），会**绕过**这道门
而"测试通过"，掩盖真实客户端的问题。复现真实客户端的门控（在 play Login 包到达时检查
服务端是否已声明通道，未声明就拒绝发送）之后，测试才有意义。

## 已验证的客户端

真机连接实测（服务端 `0.1.0-dev+26.2-26.45`，Protocol 776）：

| 客户端 | mod 版本 | 结果 |
|---|---|---|
| NeoForge 26.1 | 0.1.4 | ✅ 收到 policy 97B |
| Fabric 26.2 | 0.1.5-beta.1 | ✅ 收到 policy 97B |
| Fabric 1.17.1 | 0.1.4 | ✅ 收到 policy 97B |

三者拿到的都是同一帧 `seedGranted=true / correctionsEnabled=0 / worldgen="26.2"`，客户端
落在 `SERVER_DISABLED`。**Pumpkin 不做跨版本门禁**，1.17.1 客户端也能连上并完成握手。

## 构建与测试

```bash
./build.sh   # cargo build --release -> target/wasm32-wasip2/release/confluxmap_pumpkin.wasm
./test.sh    # 单元测试（含与参考 Java 编码器逐字节对齐的黄金向量测试）

python3 tools/test_inject_seed.py   # 种子注入脚本的回归测试
```

两者都是 Windows/Git Bash 下 re-create MSVC 环境的包装脚本：Git Bash 的 GNU coreutils
`link` 会抢在 MSVC `link.exe` 前面，导致 rustc 链接报 `link: extra operand`。Linux/macOS
或 CI 上直接 `cargo build --release` / `cargo test` 即可。

`test.sh` 用 `--target x86_64-pc-windows-msvc` 在**宿主**上跑测试：协议与配置模块是纯 Rust
且无宿主调用，API crate 生成的绑定也能编到原生 target，所以测试不必进 wasm。

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
