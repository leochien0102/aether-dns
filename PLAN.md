# aether-dns 重构计划（A：chinadns-ng 功能等价；P0 = aether-proxy profile）

> 状态：2026-11-04 规划稿。目标 A 已定；第一消费者是 aether-proxy。  
> 本计划刻意避开旧项目的失败路径：不手写 strict9、不把 fuzz/whole-unit 当门禁、不在 P0 前追全量 chinadns-ng、不并行第二个 DNS 引擎。

## 1. 目标与边界

- **目标**：用 Rust 行为等价重写 chinadns-ng；先让 aether-proxy 可替换，再按需扩大。
- **P0**：aether-proxy 当前 renderer 生成的子集；drop-in 替换现有 `chinadns-ng`。
- **P1**：全量 chinadns-ng 行为/CLI/缓存 DB/hosts/DoT/高级 no-ipv6 等；P0 稳定后逐项。
- **P2**：独立公开、多场景、未来 IPv6 DNS 扩展；不自动开工。
- **不做**：IPv6 转发/代理（aether-proxy 未来数据面）、Clash YAML/额外协议/自定义分流/多节点/服务端（属于 aether-proxy backlog）、旧项目 strict9 新政策、无 release gate 的资格测试。

## 2. P0 范围（只做这些）

P0 的基线是 `aether-proxy/src/lib/render/chinadns.uc` 与 `aether-proxy/tests/fixtures/*/expected/chinadns.conf`：

- `-C <config>` 启动；配置文件行格式；未知选项 fail closed。
- `bind-addr 127.0.0.1`、`bind-port 31053`。
- `china-dns`、`trust-dns`：T1/T2/T3；`tcp://`/`udp://`；`host@ip#port`；`tls://` 为 P0-conditional（当前默认 tcp，若 aether-proxy 配置使用则必须支持，否则 P1）。
- `gfwlist-file`、`chnlist-file`、`default-tag`（chn/none/gfw）。
- `ipset-name4` nftset 成员测试；`ipset-name6 null`。
- `add-taggfw-ip <inet@aetherproxy@gfw4>,null`。
- 自定义组 `uproxy`/`udirect`/`boot`；`group-dnl`/`group-upstream`/`group-ipset`。
- `no-ipv6`、`filter-qtype 65`（按配置条件）。
- `cache 4096`、`cache-stale 3600`、`verdict-cache 4096`（内存，无 DB）。
- 退出/信号/重启；daemon 子进程生命周期。

**P0 不做**：完整短选项集合、`cache-db`/`cache-refresh`、`hosts`/`dns-rr-ip`、`noip-as-chnip`/`fair-mode`/`reuse-port`、完整 6 自定义组、全量 DoT、IPv6 代理。

## 3. 架构

- 单 crate + 单二进制 `aether-dns`；Rust stable，edition 2024。
- **异步运行时**：Tokio current-thread 起步；每个 listener 一个任务，每查询一个 task；不做多线程/多 worker，先保证行为正确。
- **DNS 报文**：默认 `hickory-proto`；P0 的 golden/integration 验证；**不手写 strict9**。若 hickory 在 P0 行为上不匹配，先加薄适配；只有证据表明必须时才考虑最小自建解析器。
- **配置**：自定义 chinadns-ng 配置行解析器；`-C`；未知/非法值拒绝；重复按上游语义（可重复项追加、单值项最后生效）；不做完整 CLI。
- **上游**：UDP/TCP 客户端；同组并发、首个合法响应；超时/重试用 chinadns-ng 默认；`tls://` 用 rustls（若 P0 需要）。
- **路由**：列表匹配、tag、模式、default-tag、组优先级、tag:none IP 测试、首个合法响应；不顺序回退。
- **nftset**：netlink 查询成员 + 添加元素；P0 最高技术风险，先 spike。
- **缓存**：内存 LRU + TTL；stale；verdict cache；无持久化。
- **过滤**：no-ipv6/AAAA、filter-qtype 65。
- **服务**：UDP/TCP 监听 127.0.0.1:31053；信号收尾；日志到 stderr（aether-proxy 捕获）。
- **集成**：aether-proxy `chinadns_bin` 指向新二进制；保留 `chinadns-ng` 回退；P0 不改 aether-proxy 代码。版本握手（`--version` 与 aether-proxy 的最小版本检查）先在 lab 解决，不伪造。

## 4. 里程碑（粗略估算，非承诺）

| 里程碑 | 内容 | 产出 | 时间盒 |
|---|---|---|---|
| **M0 风险 spike** | nftset 成员测试/添加；hickory P0 行为；aether-proxy fixture 工具 | 三个 spike 结论 + 决策记录 | 3–5 天 |
| **M1 骨架** | config/`-C`、UDP/TCP 监听、单组单上游、转发、`--version`/`--help` | 可跑的最小二进制；golden config 全过 | 1–2 周 |
| **M2 P0 行为** | 组/列表/tag、IP 测试、nftset add、cache/stale/verdict、no-ipv6/filter | P0 行为测试全过；aether-proxy fixture 全过 | 2–3 周 |
| **M3 集成** | aether-proxy lab 替换、T1/T2/T3、模式、nftset、真机 smoke | P0 验收；回退方案 | 1 周 |
| **M4 P1（可选）** | 全量 CLI/DB/hosts/DoT/高级 no-ipv6/6 组 | 按需逐项 | 4–8 周 |

P0 总计约 **5–8 周**（一人、顺利）；M0 不通过就停，不进入 M1。

## 5. 验收

- **配置**：aether-proxy 全部 `expected/chinadns.conf` 可解析；非法配置明确失败。
- **P0 行为**：本地假上游；UDP/TCP；A/AAAA/HTTPS；模式 × 列表 × tag；组竞争；IP 测试；nftset add；cache/stale/verdict；no-ipv6/filter；超时/失败。
- **集成**：aether-proxy lab 用新二进制替换 `chinadns-ng`；T1/T2/T3、三种模式、按设备策略、nftset 写入；DNS 解析与回退不回归；保留旧二进制回退。
- **真机**：x86/ARM 至少一台；安装/启动/停止/重启；DNS 查询；nftset；24h 内不做长稳资格（P0 只要 smoke；长稳另立）。
- **不做**：fuzz/whole-unit/30 分钟作为 P0 门禁；P1 全量在 P0 前。

## 6. 风险与决策

| 风险 | 处理 |
|---|---|
| nftset 成员查询/添加（netlink） | **M0 已验证 GO**：纯 Rust netlink，test 5.5µs、batch add 50–90µs；按 IMP-004 策略（test→add→ACK→EEXIST 重试） |
| hickory 与 chinadns-ng 行为差异 | **M0 已验证 GO（薄适配）**：hickory 只解析，raw-forward + 字节偏移原地改动；不重编码 |
| 缓存/verdict 语义 | P0 先用内存 LRU，按 fixture/行为测；不做 DB；TTL 原地递减 |
| `tls://` | P0-conditional；当前默认 tcp；需要时再上 rustls |
| aether-proxy 版本检查 | lab 里解决；不伪造版本；必要时 P0 单独适配 |
| 范围蔓延 | P0 表是硬边界；P1 不在 P0 前 |

## 7. 明确不做（避免旧项目重演）

- 不手写 strict9/新安全政策；以 chinadns-ng 行为为准。
- 不做 whole-unit/cgroup/CPU/30 分钟资格；P0 只做功能/集成/真机 smoke。
- 不在 P0 前追全量 chinadns-ng；不并行第二个 DNS 引擎。
- 不复用旧项目代码；旧项目已归档，只作参考。
- 不补 aether-proxy 的 Clash/额外协议/自定义分流/多节点/服务端/IPv6；那些是它自己的 backlog。

## 8. 停止规则

- M0 spike 失败 → 停，重新评估，不进入 M1。
- M1/M2 超过时间盒且没有可演示进展 → 停，缩减 P0 或换方案。
- P0 未验收 → 不启动 P1。
- 任何“顺手”改动/新政策/资格测试 → 不做，先登记 `IMPROVEMENTS.md` 并评估。
- 不自动进入下一阶段；每个里程碑结束由所有者确认。

## 9. 第一个任务（M0）

1. **nftset spike**：用 Rust netlink 查询 `inet@aetherproxy@chnroute4` 成员、添加 `gfw4`/`uproxy4` 元素；在 netns/本机测试；记录失败方案。
2. **parser spike**：用 hickory-proto 解析 aether-proxy fixtures 的查询/响应；对比 chinadns-ng 行为；决定是否需要薄适配。
3. **fixture harness**：把 `aether-proxy/tests/fixtures/*/expected/chinadns.conf` 变成可重复的配置/行为测试入口；确定假上游方案。

M0 只出结论和最小原型，不写生产代码；结论写回本计划/`IMPROVEMENTS.md` 后再开始 M1。

## 10. M0 结果（2026-11-04）

三个 spike 均 **GO**，未触发停止条件；详见 [`M0-REPORT.md`](M0-REPORT.md)。

- **nftset**：纯 Rust netlink 完成 dump/test/add；test 5.5 µs、batch add 50–90 µs；EEXIST
  整批回滚 → 必须 test-then-add；reload 后自动恢复；无 libnftnl。策略见 IMP-004。
- **parser**：hickory-proto 0.25.2 解析 38/38 真实 wire；**禁止重编码转发**；薄适配 =
  raw-forward + 约 100–150 行字节偏移扫描（ID/RA/TC/additional/TTL）。见 IMP-005。
- **harness**：22/22 配置启动、19/19 指令覆盖、9 场景/29 查询、13/13 sanity；产出
  `coverage.json`/`baseline.json`/`wire-samples.json` 作为 M1/M2 diff 契约。见 IMP-006。

**M1 进入条件**：所有者批准 IMP-004/005/006；M1 只做骨架（config/`-C`、UDP/TCP 监听、
单组单上游转发、`--version`/`--help`）。验收是 22/22 配置启动 + A/AAAA/HTTPS×UDP/TCP 转发矩阵；
baseline diff 属于 M2；保留 chinadns-ng 回退；不扩 P1。

**2026-11-04 所有者确认**：D1–D6 通过（M1 启动、IMP-004/005/006 批准、git+worktree、
M1 纯转发到第一个 trust-dns、日期版本 `2026.11.04`、Tokio current-thread、M1 验收独立脚本）。
任务卡见 [`docs/tasks/m1.md`](docs/tasks/m1.md)；T1.0/T1.1/T1.6 已完成，T1.2/T1.3 进行中；
T1.4 等 T1.2+T1.3，T1.5/T1.7 随后，T1.8/T1.9 最后。

**2026-11-04 复审**：T1.0（仓库与构建骨架）通过，提交 `bc6c16d`；`tests/review.sh`
在干净 PATH 下通过（rustfmt/clippy 已装）。T1.1（worktree 脚本）通过，提交 `26c8f6e`；
`start-task.sh` 的创建/复用/脏检出拒绝/卡号校验均实测。T1.6（CLI 版本/帮助）通过并合入
`938f0cc`；T1.2/T1.3 继续，合并后发 T1.4。

**M0 用时**：2026-11-04 单日完成，低于 3–5 天盒子。**M0 结束即停，不自动进入 M1。**
