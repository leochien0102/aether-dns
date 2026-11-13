# aether-dns（新起点）

目标：**A：Rust 功能等价重写 chinadns-ng**；第一消费者是 aether-proxy；先 P0，后 P1/P2。

- [CHARTER.md](CHARTER.md)：目标、边界、消费者/阶段、改进评估门、停止规则。
- [PLAN.md](PLAN.md)：重构计划（P0/P1/P2、架构、里程碑、风险、验收）。
- [RESEARCH-2026-11-04.md](RESEARCH-2026-11-04.md)：上游/helloworld/aether-proxy profile 研究。
- [PARITY-MATRIX.md](PARITY-MATRIX.md)：功能/行为对照矩阵 + P0 profile。
- [IMPROVEMENTS.md](IMPROVEMENTS.md)：改进/修正登记（未批准不实现）。
- [M0-REPORT.md](M0-REPORT.md)：M0 风险 spike 报告（nftset/parser/fixture harness，go/no-go）。
- [M1-REPORT.md](M1-REPORT.md)：M1 可跑骨架报告（49 测试、22/22 启动、6/6 转发矩阵、已知差距）。
- [M2-REPORT.md](M2-REPORT.md)：M2 P0 行为报告（210 测试、baseline 29/29、13/13 sanity、已知差距）。
- [M3-REPORT.md](M3-REPORT.md)：M3 真机集成报告（H28K 11.8h 观察、146 样本、重启期三缺陷转 M4-04/05/06）。
- [spikes/](spikes/)：M0 最小原型与可复跑证据（非生产代码）。
- [docs/tasks/](docs/tasks/README.md)：任务卡与执行约定（M1–M3 完成；M4 进行中）。
- [M3 真机检查表](docs/tasks/m3.md)：M3-01–06 全部完成（2026-11-06）。
- [独立会话交接](docs/handoffs/session-handoff-20261109.md)：当前源码/版本/设备最后记录、跨项目归属与新对话入口。
- [M4 任务](docs/tasks/m4.md)：M4-01/07 已交付；M4-04/05相关proxy工作已交付，其他候选未冻结。
- [M4 构建安装记录](M4-DEPLOYMENT-2026-11-07.md)：H28K 已安装缓存刷新包；未重启设备。
- [M4 真机验收](M4-VALIDATION-2026-11-07.md)：缓存刷新和T15.1本轮真机指标通过，列明观测范围与限制。

旧项目已停止并归档：

- 完整树：`../archive/aether-dns-2026-11-04/`
- 便携包：`../archive/aether-dns-2026-11-04-src.tar.gz`
- 停止记录：`../archive/aether-dns-2026-11-04/STOP.md`

## 日志流约定（必须保持）

| 流 | 内容 | 固件里被记成 |
|---|---|---|
| stdout | 生命周期与信息：`listening UDP/TCP …`、`shutdown: draining queries …`、`upstream failed for client …`（可恢复的诊断） | `daemon.info` |
| stderr | 真实错误：`UDP receive:` / `TCP accept:` / `TCP client` / `UDP send`、nftset 写入失败、IP 测试失败、配置与用法错误 | `daemon.err` |

原因：aether-dns 作为 aether-proxy 的子进程运行，**procd 把子进程 stderr 记为 `daemon.err`、stdout 记为 `daemon.info`**。生命周期行若写在 stderr，`logread | grep err` 里就是假错误（2026-11-13 真机观测到 `listening TCP/UDP` 与 `shutdown: draining` 被记成 `daemon.err`）。

两条硬规则：

1. **文本不得随流改动**：代理侧 `docs/09` 按文本分类日志，改文案会破坏判别规则；换流时只换宏（`eprintln!` ↔ `println!`），参数字面量保持原样。
2. **错误仍走 stderr**：可恢复的诊断（上游超时/重置，已回 SERVFAIL）算信息；socket 级失败、配置错误、用法错误算错误。

测试侧由 `tests/common/mod.rs` 的 `Server` 同时捕获两个流：`info`（stdout）用于就绪/生命周期断言，`errors`（stderr）用于错误断言；`tests/log_streams.rs` 是这条约定的回归测试。

当前状态（2026-11-11）：**1.0.0 已公开发布**（tag `v1.0.0`，四个静态 musl 资产 + `SHA256SUMS`，仓库 public）。尚未部署到任何设备，H28K 仍是 `2026.11.04-1`；proxy 按引擎闸门未落地前禁止部署 1.0.0。M4-07测试修复、M4-01缓存刷新、M4-03版本迁移、M4-08日志可诊断性、M4-09包定义、M4-10发布流水线、M4-11日志流优先级均已合并验收。
启动期解环、owned readiness、handoff及init契约已由aether-proxy后续T15系列交付并发布r7，不重做S15.2探针；命名、中断体验和其他parity仍按候选分别决策。独立semver已在DNS仓库落地；打包及proxy按引擎版本闸门另卡处理，闸门拆分前不得部署 `1.0.0`。Cargo版本、二进制版本和设备opkg版本不能混为同号发行，详见[独立交接](docs/handoffs/session-handoff-20261109.md)。
M3 的安装/切换/回滚/模式/T级/短时观察结果见 [M3-REPORT.md](M3-REPORT.md)。
