# 改进/修正登记（未批准不实现）

> 默认行为与 chinadns-ng `match`。任何优化、修正或有意差异，先评估、再批准、再实现。
> 没有批准的条目不得写代码，也不得用“顺手”方式进入实现。
> 项目级理由（Rust 重写、长期维护）不是功能差异，不替代逐项 parity 验收。

## 评估门

每个条目必须包含：

1. **问题/机会**：具体是什么，不是“感觉可以更好”。
2. **证据**：上游 issue/README、helloworld 变更、真实配置、可复现测试或新网络环境。
3. **变更**：准备改什么，是否改变默认行为。
4. **parity 影响**：`compatible` / `intentionally differ` / `breaking`。
5. **成本/风险**：实现、测试、迁移、维护成本；回滚方式。
6. **验收**：怎么证明改对了，且没有破坏原功能。
7. **决策**：`pending` / `approved` / `rejected`，所有者与日期。

## 初始候选（待研究，不是承诺）

### IMP-001 OpenWrt/SSR+ 集成方式（pending）

- 问题/机会：chinadns-ng 在 `luci-app-ssr-plus` 中是可选 include；有 mosdns/dnsproxy/dns2socks 等替代。新的 Rust 组件如何被 OpenWrt/SSR+ 选择、打包、启动，需要明确。
- 证据：[SSR+ Makefile](https://raw.githubusercontent.com/fw876/helloworld/master/luci-app-ssr-plus/Makefile) 的 `INCLUDE_ChinaDNS_NG` 可选依赖；[上游 README](https://raw.githubusercontent.com/zfl9/chinadns-ng/master/README.md) 的 OpenWrt 段。
- 变更：提供可选包/service，是否作为默认后端待定。
- parity 影响：`compatible`（集成层），不改变 DNS 行为。
- 成本/风险：包维护、启动脚本、配置映射；需单独验收。
- 测试/回滚：OpenWrt 安装/启动/停止/升级；可移除包。
- 决策：pending。

### IMP-002 默认上游/新网络环境（pending）

- 问题/机会：上游默认 `114 DNS`/`Google DNS`；新网络环境可能需要不同默认，但改默认会影响行为。
- 证据：`[HELP]` 默认值；尚无新环境的具体失败证据。
- 变更：保持默认或提供配置；不先改默认。
- parity 影响：若改默认为 `intentionally differ`；不改则 `compatible`。
- 成本/风险：行为变化、用户迁移；需证据。
- 测试/回滚：默认值/配置测试；可恢复。
- 决策：pending。

### IMP-003 DoT 实现选择（pending）

- 问题/机会：上游用 wolfssl；Rust 侧可能用 rustls。实现不同，但 DoT 行为/证书校验应对齐。
- 证据：上游 README 的 `cert-verify`/`ca-certs`/wolfssl 构建。
- 变更：实现选型，不改变默认行为。
- parity 影响：`compatible`；若证书校验策略不同则 `intentionally differ`。
- 成本/风险：证书/平台/依赖；需测试。
- 测试/回滚：DoT 连接/证书失败/超时；可回退。
- 决策：pending。

### IMP-004 nftset 更新策略与 nft CLI 互操作（pending）

- 问题/机会：M0 证明纯 Rust netlink 可行，但需要明确写入策略：先测后加、batch 原子失败、
  并发 EEXIST、失败日志；另外直接 netlink 写入的 interval 不会被 nft 用户态 auto-merge，
  在某些相邻区间组合下 `nft add element` 会报 ENOENT。
- 证据：`M0-REPORT.md` §2；`spikes/m0-nftset/evidence.log`；用真实 chinadns-ng
  2025.08.09 复现同样现象；aether-proxy 的 `sets_export` 用 `nft -j list set`，
  reload 用整表 `nft -f`，不使用 `nft add element`（`apply.uc`、`reload.uc`）。
- 变更：aether-dns 采用逐 IP `GETSETELEM` 测试 → 批量 `NEWSETELEM` → 单 ACK →
  遇 `-EEXIST` 重试；不缓存 set handle；`-EPERM`/`-ENOENT` 记日志并按“不在集合”处理；
  同一进程串行化写。不复刻 nft 的 auto-merge，也不依赖 nft 用户态合并。
- parity 影响：`compatible`（集合内容与 chinadns-ng 一致；nft CLI 互操作是上游既有行为）。
- 成本/风险：低；并发窗口需要重试；集合内容可能是未合并区间（与上游一致）。
- 测试/回滚：M0 的 dump/test/add/EEXIST/重试/失败模式 + harness 的 gfw4/uproxy4/udirect4 断言；
  可回退到 chinadns-ng。
- 决策：pending（M1 前批准）。

### IMP-005 转发路径：raw-forward + 字节偏移薄适配（pending）

- 问题/机会：hickory-proto 重编码会改变字节（压缩、丢尾部、多 question 畸形、保留
  chinadns-ng 会裁掉的 OPT），不能用于转发；但解析、分类、缓存仍需要 hickory。
- 证据：`M0-REPORT.md` §3；`spikes/m0-parser/evidence-*.log`；
  `spikes/m0-fixtures/wire-samples.json`；chinadns-ng `dns.c`/`CacheMsg.zig` 的原地改动。
- 变更：hickory 只解析；转发原始字节；约 100–150 行偏移扫描器做
  header 原地改写（ID/RA/TC）、additional 裁剪、TTL 原地递减、NODATA 合成；
  不手写完整 DNS parser，不做业务逻辑重编码。
- parity 影响：`compatible`；畸形包的边界差异列为 known divergence（不阻塞 P0）。
- 成本/风险：低–中；偏移扫描需覆盖压缩指针/异常包；用 wire 样本回归。
- 测试/回滚：M0 的 20 例合成 + 38 个真实 wire + 8 个边界包；可回退到 chinadns-ng。
- 决策：pending（M1 前批准）。

### IMP-006 fixture harness / baseline 作为 M1/M2 验收入口（pending）

- 问题/机会：P0 需要可重复的行为验收，不能靠人工看配置。
- 证据：`M0-REPORT.md` §4；`spikes/m0-fixtures/coverage.json`、`startup.json`、
  `baseline.json`、`wire-samples.json`。
- 变更：保留 harness 作为测试工具（Python 或 M1 改写成 Rust 测试），以 `baseline.json`
  为 diff 契约；新增 fixture 时扩展场景。是否在 M1 重写为 Rust 需要所有者决定。
- parity 影响：测试基础设施，不改变运行时行为。
- 成本/风险：中（维护两套语言）；netns/root 只用于 lab/CI。
- 测试/回滚：用同一 harness 把 SUT 从 chinadns-ng 换成 aether-dns，逐字段 diff。
- 决策：pending（M1 前批准）。

### IMP-007 上游失败日志指认真实上游（approved 2026-11-10）

- 问题/机会：失败日志把客户端地址标成 upstream 且只剩 last error，导致跨项目误判（2026-11-10 10:40 的 H28K 事件）。
- 证据：`src/server.rs:302`（`peer` 是客户端）、`src/upstream_group.rs:34,41`（`last_error` 覆盖）；proxy 侧 `docs/09` 的读法补充。
- 变更：组层聚合 `(上游, 原因)`，服务端标签改为 `upstream failed for client …`。
- parity 影响：`compatible`（只改 stderr 文本，不改解析/路由/缓存/应答语义）。
- 成本/风险：低；`ErrorKind` 保持最后一个错误，避免动既有测试语义。
- 测试/回滚：`tests/upstream_group.rs` 新用例 + `tests/upstream_log.rs`；回滚即还原两处文本。
- 决策：approved（所有者 2026-11-10）。

## 明确不登记为“缺陷”的事项

- **helloworld 不再必选 chinadns-ng**：抓到的证据是 SSR+ 把它作为**可选** include，并提供 mosdns/dnsproxy/dns2socks 等替代；**没有证据**表明是 chinadns-ng 的技术缺陷。具体原因需查 issue/提交；不要据此提出“修复”。
- **上游长期无提交**：2025-08-09 至 2026-11-04 约 15 个月；这是维护风险/项目理由，不是某个功能缺陷。Rust 重写是项目级决定，不自动改变行为。
- **`nft add element` 在 chinadns-ng 写过的集合上失败**：M0 用真实 chinadns-ng 复现；直接 netlink 的 interval 不会被 nft 用户态 auto-merge，导致某些相邻区间组合下 `nft add element` 报 ENOENT。aether-proxy 的导出/重建路径不依赖该命令。不是 aether-dns 的回归，按 IMP-004 作为已知约束处理，不修上游行为。
