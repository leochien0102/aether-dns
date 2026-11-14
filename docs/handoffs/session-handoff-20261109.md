# aether-dns 独立会话交接（2026-11-09）

> 本文件用于从混合会话切换到**仅负责aether-dns**的新对话。本体M0–M3和已批准的M4缓存/测试工作已交付；没有默认待执行新卡。aether-proxy刚发布r7，不代表DNS引擎也升级、需要重建或版本同步。

## 1. 权威状态与版本区别

| 对象 | 已核实状态 |
|---|---|
| 项目目录 | `/workspace/openwrt/aether-dns`（当前新起点，非archive旧项目） |
| 交接文档创建前main | `8e2713b4639b61127f5f276045f267ce68257977`，工作树干净 |
| Git远端/tag/worktree | **origin = `github.com/leochien0102/aether-dns`（2026-11-11 起 public）**；tag `v1.0.0`、`v1.0.1`、`v1.0.2`；旧设计史保留在分支 `design-20261102`；worktree list 只有主树 |
| 实际设备构建源码 | `325b614a7bb927cc94dc6d7b38751010e9be8531`，M4-01接受合并 |
| B3固定真实引擎测试源码 | `a3d42a278ce84ba5ed54988a9d30a695b7ec97b5`，其自身为后续设计文档提交 |
| 当前生产代码差异 | 相对 325b614 仅 5 个文件：`src/main.rs` banner 改由 crate 版本派生（M4-03）、`src/server.rs`+`src/upstream_group.rs` 上游失败日志（M4-08）、`Cargo.toml`/`Cargo.lock` 版本字段；查询路由/缓存/配置逻辑无改动，**无需为 proxy r7 重建 DNS** |
| Cargo package.version | **1.0.2**（M4-13 发布准备），与 `openwrt/Makefile` 核心由 `tests/ci/check-version.sh` 守卫 |
| 二进制-V第二字段 | **1.0.2**，完整输出仅 `aether-dns 1.0.2`，由 Cargo 派生；按所有者指示不带修订（非设备已装版本） |
| OpenWrt 包版本 | **1.0.2-r1**（`PKG_VERSION=1.0.2`、`PKG_RELEASE=1`）；本仓库配方仅打包本地二进制，feeds 配方迁至 proxy 仓库 |
| **公开发布（2026-11-14）** | `v1.0.2` 已发布，七资产：`aether-dns_1.0.2-r1_{aarch64_generic,x86_64}.ipk`、`aether-dns-1.0.2-r1_{aarch64_generic,x86_64}.apk`、无扩展名 `aether-dns-1.0.2-r1-{aarch64_generic,x86_64}`、`SHA256SUMS`（覆盖六资产）。**裸二进制 sha256**：`9c21f7d9d35a2b9784719bebb17f88dce3fcba0b446acb06bb38cbec4c95e223`（aarch64_generic，2277296 B）、`d7ad176292664bb5f4d070d3d3ed98b483c2403fc6ef87efa1ff1e0483a499eb`（x86_64，2148192 B）。Release 不保留可执行位，消费方（proxy 的 `aether-dns/Makefile`）下载后须 `chmod +x` |
| 已公开发布 | `v1.0.0`、`v1.0.1` 各四包 + `SHA256SUMS`；`v1.0.2` 起额外提供裸二进制。资产重传会被拒绝（Release 已存在即失败） |
| H28K本地opkg包 | 设备侧 2026-11-13 真机日志观测为 **`aether-dns 1.0.0-r1`**；`1.0.2` 尚未部署 |

本文件及入口更新是新增的**本地文档提交，未推送、不构建、不测试**；「无 remote」的旧结论已于 2026-11-11 作废，改为上面的公开发布状态。不要从目录名、Cargo版本或proxy tag猜测DNS发行状态。

## 2. 产品范围：P0已交付不等于全量parity已完成

最终目标A是Rust行为等价重写chinadns-ng；先做aether-proxy当前renderer消费者子集，再按批准范围逐项扩展。章程中的“从干净起点、先矩阵/0代码”是已执行的2026-11-04起始顺序，不能因新开对话就清空现仓库、重回M0或重写已交付切片。必读[章程](../../CHARTER.md)、[计划](../../PLAN.md)、[对照矩阵](../../PARITY-MATRIX.md)、[改进登记](../../IMPROVEMENTS.md)。

- M0–M3已完成；M3包括集成、安装/回退、模式/T级和阶段观察，详见[M3报告](../../M3-REPORT.md)。
- **M4-07 Rust测试harness端口竞争**已修：有界重试最多10次，失败kill/wait并清conf，不放松业务断言。合并b2acecf；不是proxy T15.9 Python Authority的16次双socket分配。
- **M4-01缓存刷新**已完成：实现ec69b4e，最终构建/接受源码325b614。fresh剩余TTL<=ttl/3且TTL>=10秒时refresh-ahead；stale仍立即回TTL0并刷新；同key在途去重、并发4、失败冷却5秒，成功仍经add校验，shutdown不被刷新阻塞。不是定时扫缓存或禁止所有TTL0。
- 最终本地review FAIL=0，原始记录20个测试目标合计224项；这是当时验收，不冒称今天重跑全套。
- 全量chinadns-ng、DoT/证书策略、负缓存、更多IPv6 DNS/sets、cache-db/hosts/高级选项等**未全部交付**。按矩阵区分unsupported/improve/differ，不把候选当已承诺代码缺陷，也不自动开工。
- 不做递归/权威DNS、DNSSEC验证器、DHCP或IPv6流量代理；DNS的AAAA/IPv6监听/上游等与proxy的IPv6数据面不是同一需求。

旧`archive/aether-dns-2026-11-04`已停止，不是本项目基线；撤回的8264c51/b4909f0也不从中补回代码。跨仓库提交号写清仓库，不能把aether-dns@cb28b84缓存卡冻结提交当proxy r6基线。

## 3. 最后已验设备与缓存证据

本次交接整理不访问设备；以下来自已归档记录，不保证新会话此刻现场仍相同。

- H28K入口`root@192.168.5.254`，WSL现有SSH密钥；实际路径`/usr/bin/aether-dns`。在proxy的LAN链中，dnsmasq接53，转到本机31053；T1的trust组经本机Xray TCP tunnel31082/31083，china组/节点引导等直连选择另按配置，不把所有UDP上游都视为泄漏。
- ARM64运行二进制SHA256：`ba17e8b9401c0183360148555a59e2b6b93ba651e4df01bd360452e9f4caa3b9`；ipk SHA256：`0ef0212c53071f5eb402cd02d51d8e91f981ccce0143efd4231c970208a7dabf`。
- 不要把B3本地x86引擎ce09…哈希当H28K ARM哈希；不同架构/构建不能直接逐字节比较。
- 2026-11-07 TTL直接查31053绕过dnsmasq缓存：61/61 NOERROR，TTL0=1.64%，符合<5%。持续5qps/360秒：1800/1800 NOERROR、TTL0=0；上游120次/360秒=0.333次/秒是**含其他请求、多上游和重传的保守上界**，不是example.com精确刷新次数。
- 并发命中10次上游计数无增量；未在设备额外注入冷却/并发上限失败，相关证据来自本地受控测试。不能把这组指标外推成所有域名/长期负载保证。
- 2026-11-09 proxy r6.t1510验收再次核对DNS二进制哈希未变，恢复running/T1，boot ID `97edbacd-fd63-4a21-beb7-0cc59ac22b93`未变。proxy正式r7发布阶段没有再安装或查询设备。

详情见[M4安装](../../M4-DEPLOYMENT-2026-11-07.md)、[M4真机指标](../../M4-VALIDATION-2026-11-07.md)。设备旧DNS备份在`/root/aether-dns-before-m4-20261107/`，需要回退时先获得用户授权，不因文档带有旧命令就执行。

## 4. 构建与质量门

- 本仓库Cargo工程可构建本体，但**没有受Git跟踪的OpenWrt Makefile**。
- 历史本机SDK配方仍存在：`/workspace/openwrt/openwrt-actions/build/rockchip/package/aether-dns-local/Makefile`；本次已只读确认。它是源固定的本地M4配方，使用其source快照，不会自动跟随本库新main，也不是已发布feed。
- 历史交叉构建使用1.99.1宿主Rust、独立target std sysroot、aarch64-openwrt-linux-musl-gcc，不改系统Rust/rustup。质量门PATH工具与该交叉工具链不是同一概念，实际新构建前按记录核对，不盲换工具链。
- 没有新生产代码就不为proxy发版重复构建。若以后确需新包，先固定源码、核对BUILD-INFO/旧快照和工具链、只编单包，不刷固件/feeds update；隔离输出和回退另按授权准备。
- 新实现卡的门禁：cargo fmt、clippy、test以及本库tests/review.sh，具体以[执行约定](../tasks/README.md)和冻结卡为准。纯交接文档不重跑这些套件，不改runner放宽断言。

## 5. 跨项目工作归属（防止再次混做）

| 事项 | 归属/当前结论 |
|---|---|
| M4-02 aether-*逻辑命名 | **主要是proxy**的UI/日志/状态JSON及兼容迁移；DNS不是靠改stdout或加chinadns别名来代做 |
| M4-03独立semver | DNS定义自己的产品版本；proxy按具体引擎检查最低值/范围。需两项目先约定，不能独立改号就装 |
| M4-04 restart handoff | proxy T15.1及后续init契约，已交付；不在Rust server里延时/重试来代替 |
| M4-05启动SERVFAIL/解环 | proxy B1/B2/B3/T15.7已交付并纳入正式r7；不要重做S15.2探针或Rust重写proxy解析器 |
| M4-06中断体验 | proxy候选，需重估；“两次dnsmasq重启”是历史，现restart仅安装阶段一次，不自动承诺零中断 |

proxy已正式发布0.6.3-r7，默认包依赖仍是aether-xray＋chinadns-ng，**没有新增aether-dns发布包**。参考[proxy独立交接](../../../aether-proxy/docs/handoffs/session-handoff-20261109.md)即可，不在DNS会话修改其main/channel/版本或设备启停脚本。

## 6. 待决策，而非已冻结任务

1. ~~M4-03 版本迁移~~：**已完成**（`1.0.0` → `1.0.1` → `1.0.2`，当前 tag `v1.0.2`；Cargo 版本、`-V` 核心、包版本由 `tests/ci/check-version.sh` 守卫）。日期版本到 semver 的 opkg 排序问题随一次性手动重装解决，不再适用。
2. ~~公开独立发布~~：**已完成**（`leochien0102/aether-dns` 自 2026-11-11 起 public；CI 由 tag 触发；同一 tag 的 Release 资产重传会被拒绝）。远端与签名仍归本仓库自管，不挪用 proxy-channel 的密钥或仓库。
3. 本体其他候选见[M4](../tasks/m4.md)。一次只冻结一个有量化目标的切片；DoT、负缓存、question匹配、TCP连接上限等不合并成一次大改。按章程先登记并批准improve/differ。
4. M4-02/06等proxy候选转其独立对话；跨项目接口先只读评估并由用户确认双方分工，不默认同时改两库。
5. **下一张 DNS 发布卡必须包含**（2026-11-14 复核，按所有者决定不返工 1.0.2）：
   `tests/ci/verify-assets.sh` 的裸二进制权限断言改为 `stat.S_IMODE(path.stat().st_mode) == 0o755`；
   负向用例补 `0777` 与 `02755` 各一条。
   背景：现有 `path.stat().st_mode & 0o755 == 0o755` **不是**优先级 bug（Python 的 `&` 高于 `==`，
   实际是 `(st_mode & 0o755) == 0o755`），语义为"至少满足 rwxr-xr-x"；因此 `0777`、`02755`、`04755`
   这类多余位会通过。`S_IMODE` 掩到 `0o7777`，可挡住 setuid/setgid/sticky 与世界可写。

## 7. 网络观察与测试资源

**WSL＋NAT环境相关UDP观察已归档，具体归因未定。** NAT600中5超时，Windows原生/mirrored/Debian桥接各600/600成功；仅其他独立环境复现相同问题才重开，不列为DNS已知故障或常规验收阻断。对照通过不等于软件所有UDP路径无缺陷；仍需真实UDP功能覆盖，不能用TCP代替。WSL内部单元/loopback/受控集成仍有效，不擅自改mirrored、重启WSL或动Docker/Traefik。

VMware Debian 13资源：`leochien@192.168.139.130`，现有WSL密钥SSH、sudo免密码；2CPU/约4GB，内核6.12.116，Python3.13、Bash/Dash/BusyBox ash。缺Git/编译工具/Rust/tcpdump，不能当作完整Rust构建机。用户声明快照**baseline-new-install-261109**；只读调查已完成，未安装软件或跑功能测试，未授权自动恢复/删除快照。NAT网关192.168.139.2，适合按需本地专项，不把跨NAT UDP作为唯一故障依据。

旧VirtualBox ag-t04-debian不再使用，不为核查被中止的持久化而启动它。历史桥接成功数据继续归档，不重做。

## 8. 证据、角色与新会话停点

- DNS本体证据：本库`dist/logs/m4-nightly/`、`dist/logs/t15.1-install/`等忽略目录；正式摘要以上述M4安装/验收文档为准。
- **同一dist/logs也存有proxy会话证据**（t1510-h28k/release-0.6.3-r7等），不能据目录归属猜测DNS改动/发版，也不要迁移或删历史证据。
- 你负责证据化设计、拆卡、复审和授权后合并交付；用户转发实现者，不自动派实现子智能体或代写卡内生产代码。实现者只在本库start-task创建的卡内worktree执行，跨项目只读引用必须遵本卡。
- 本库M4部分段落/旧M1入口是历史目标，不是重做授权；读当前状态与新冻结卡再动手。没有新目标就只读确认，不开工、不跑全套、不构建/部署。
- **不擅自重启H28K/刷固件/动WAN接口**；安装、服务窗口与回退需用户新授权。不猜密码、改密钥或为测试擅自装软件。
- 工程文件在WSL，按新会话实际工具/权限工作；旧会话曾用过的权限模式不继承为授权。权限拒绝就报告，不换工具绕过。

### 可粘贴入口

```text
本对话仅负责aether-dns。
先读 @/workspace/openwrt/aether-dns/docs/handoffs/session-handoff-20261109.md，再读本库章程、对照矩阵和执行约定。
只读核对Git/版本/待决策项，简要确认后等我指定下一目标。
不要自动构建、跑全套、改版本、加远端、推送或操作设备；不改aether-proxy/channel。
沿用用户转发实现、你负责设计/复审的分工。旧混合会话及目标已结束，不恢复旧目标。
```
