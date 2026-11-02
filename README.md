# aether-dns

面向 OpenWrt 的轻量 DNS 策略转发器，计划使用 Rust 实现。

**状态：方案草案，等待交叉审查。尚无运行代码，不可替换现有 DNS 服务。**

- **已定方向**：独立项目、优先 Rust；DNS 与未来多出口分流分别推进。
- **建议待批**：协议行为、兼容层、控制接口、资源预算与项目许可证。
- **候选待验证**：Tokio、DNS 协议库、netlink 后端及目标平台构建。

## 为什么建立这个项目

aether-proxy 的基本功能已经可用，多出口分流不是当前紧急目标。现有 ChinaDNS-NG 提供的域名分类、国内/可信应答选择、TCP 上游、缓存和 nft 集成值得保留，但长期维护不能只依赖上游继续更新。

本项目拟按明确的行为规格重新实现当前需要的能力，建立可测试、可维护、可扩展的独立组件。不是立即逐行翻译 ChinaDNS-NG，也不因为项目长期没有提交就认定它存在已知漏洞。

## 设计入口

| 文件 | 内容 |
|---|---|
| [docs/01-proposal.md](docs/01-proposal.md) | 范围、架构、Rust 选型、协议行为、缓存、配置重载与资源限制 |
| [docs/02-compatibility.md](docs/02-compatibility.md) | 与当前 aether-proxy 的兼容矩阵、语义差异、nft 集成与迁移 |
| [docs/03-validation-plan.md](docs/03-validation-plan.md) | 原型、测试、资源预算、构建发布及阶段验收 |
| [docs/04-review.md](docs/04-review.md) | 决策状态、待确认问题和交叉审查清单 |
| [docs/05-evidence.md](docs/05-evidence.md) | 调研依据、已知事实与未经验证的假设 |
| [docs/review/2026-11-02-response.md](docs/review/2026-11-02-response.md) | 第一轮交叉审查逐项处理、异议证据及剩余阻断 |

建议按上表顺序阅读。标记为“建议”的条款尚未获所有者批准；未验证的性能数字是验收候选预算，不是实测结论。

## 核心边界

```text
系统 dnsmasq（DHCP / 本地域名）
    → aether-dns（DNS 策略、转发、缓存、必要的 nft 兼容）
        ├─ 国内 DNS
        ├─ 独立的引导 DNS
        └─ Xray 本地 tunnel 入站 → 节点 → 可信 DNS
```

- 不接管 DHCP，不修改系统 dnsmasq/firewall/network 的持久配置。
- 不实现代理协议、流量分流、节点测速或 T1/T2/T3 的健康决策。
- 不把未来分流建立在 DNS 动态 IP 集合这一种信息来源上。
- 现有 ChinaDNS-NG 保持在用，直到新实现满足替换门槛。
- 通用 DNS 核心不依赖 nft；形态 A 的分类/写集合属于可选兼容层。形态 B 尚未定稿，不能预先认定这些能力全部作废。

## 开发与许可状态

本次仅建立方案文档，没有 Cargo 工程、工具链版本或依赖锁定，因此尚无构建/测试命令。

实现许可证待所有者审定。本仓库当前不附带开源授权声明，也未引入第三方实现代码。计划采用独立实现；若复制、翻译或改编 ChinaDNS-NG 的受版权保护代码，不能因改用 Rust 而消除其 AGPL 义务。审定前不得引入该实现代码。详见 [证据与许可边界](docs/05-evidence.md)。
