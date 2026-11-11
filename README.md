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

当前状态（M4-03）：**仓库版本已迁到 `1.0.0`**，Cargo 版本与二进制 `-V` 第二字段一致；尚未打包、未部署。M4-07测试修复、M4-01缓存刷新已合并并验收；H28K最后已验包仍为`2026.11.04-1`，设备未变。
启动期解环、owned readiness、handoff及init契约已由aether-proxy后续T15系列交付并发布r7，不重做S15.2探针；命名、中断体验和其他parity仍按候选分别决策。独立semver已在DNS仓库落地；打包及proxy按引擎版本闸门另卡处理，闸门拆分前不得部署 `1.0.0`。Cargo版本、二进制版本和设备opkg版本不能混为同号发行，详见[独立交接](docs/handoffs/session-handoff-20261109.md)。
M3 的安装/切换/回滚/模式/T级/短时观察结果见 [M3-REPORT.md](M3-REPORT.md)。
