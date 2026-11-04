# aether-dns — 项目章程（A：Rust 功能等价重写 chinadns-ng）

## 状态

- 2026-11-04 所有者选定 **A**。
- 旧项目已停止并归档：`/workspace/openwrt/archive/aether-dns-2026-11-04/`。
- 旧代码**不是本项目的基线**；只有本章程明确引用才复用。
- 本项目从干净目录开始；先做对照矩阵，不写代码。

## 目标

- 用 Rust 重写 chinadns-ng，保留用户依赖的功能与行为；可维护、可扩展。
- **行为等价，不是逐行照抄**：实现可以现代化，但默认行为必须能与 chinadns-ng 对照。
- 允许在评估后做优化/修正；不得静默改变行为。

## 非目标

- 递归解析器、权威 DNS、DNSSEC 验证器、DHCP。
- 全新协议设计；未经评估的功能扩张。
- 直接复制旧项目代码；旧项目只作参考/归档。
- **IPv6 转发/代理**：属于 aether-proxy 的未来数据面；aether-dns 只做 DNS 解析/分流，不实现 IPv6 代理。IPv6 DNS（AAAA、IPv6 上游/监听、ipset6/nftset、可配置 no-ipv6）仍是 parity 的一部分。

## 等价与改进

- **基线**：chinadns-ng 的文档、配置、可观察行为（README、源码、issue、helloworld 的实际使用方式）。
- 每项功能在 `PARITY-MATRIX.md` 标记：`match` / `improve` / `intentionally differ` / `unsupported`。
- 任何 `improve` / `differ` 必须先写 `IMPROVEMENTS.md`：问题证据、影响、成本/风险、测试、回滚、所有者批准；默认 `match`。
- **helloworld 不再把 chinadns-ng 作为必选组件**：先查清原因（技术、维护、替代、政策），作为矩阵输入；不猜。
- 不允许“顺手改”；每项偏离都要有记录和批准。

## 消费者与阶段

- **第一消费者**：aether-proxy（Xray-only、自用；当前不推广）。
- **P0**：只做 aether-proxy 当前 renderer 生成的 chinadns-ng 子集；用 `aether-proxy/tests/fixtures/*/expected/chinadns.conf` 做 golden 对照；保留 chinadns-ng 作为回退，直到 P0 通过。
- **P1**：全量 chinadns-ng 行为/CLI/缓存 DB/hosts/DoT/高级 no-ipv6 等；按需逐项。
- **P2**：独立公开使用/更多场景/未来 IPv6 DNS 扩展；不自动开工。
- aether-proxy 的 Clash YAML、SSR/TUIC/Naive/ShadowTLS、自定义分流、多节点、服务端、IPv6 代理属于其自身 backlog；当前自用/Xray-only 没有硬缺失，不在本项目的 P0。

## 里程碑

1. 章程 + `PARITY-MATRIX.md` + `IMPROVEMENTS.md` + 上游/helloworld 原因研究；**0 代码、0 测试**。
2. 第一个可交付切片；逐项验收。
3. OpenWrt/真机/长稳；另立预算。

## 停止规则

- 矩阵/原因未完成前不写代码。
- 每轮只做一件事；没有量化目标不开工。
- 未批准的 `improve` / `differ` 不实现。
- 旧项目不自动复用；不再做旧项目的资格微测试。
