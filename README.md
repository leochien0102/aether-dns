# aether-dns（新起点）

目标：**A：Rust 功能等价重写 chinadns-ng**；第一消费者是 aether-proxy；先 P0，后 P1/P2。

- [CHARTER.md](CHARTER.md)：目标、边界、消费者/阶段、改进评估门、停止规则。
- [PLAN.md](PLAN.md)：重构计划（P0/P1/P2、架构、里程碑、风险、验收）。
- [RESEARCH-2026-11-04.md](RESEARCH-2026-11-04.md)：上游/helloworld/aether-proxy profile 研究。
- [PARITY-MATRIX.md](PARITY-MATRIX.md)：功能/行为对照矩阵 + P0 profile。
- [IMPROVEMENTS.md](IMPROVEMENTS.md)：改进/修正登记（未批准不实现）。
- [M0-REPORT.md](M0-REPORT.md)：M0 风险 spike 报告（nftset/parser/fixture harness，go/no-go）。
- [spikes/](spikes/)：M0 最小原型与可复跑证据（非生产代码）。
- [docs/tasks/](docs/tasks/README.md)：任务卡与执行约定（M1 已确认，T1.0/T1.1 开始执行）。

旧项目已停止并归档：

- 完整树：`../archive/aether-dns-2026-11-04/`
- 便携包：`../archive/aether-dns-2026-11-04-src.tar.gz`
- 停止记录：`../archive/aether-dns-2026-11-04/STOP.md`

当前阶段：**M0 完成（2026-11-04）**；M1 进行中，T1.0/T1.1/T1.2/T1.3/T1.6 已复审通过，T1.4 进行中。
生产代码仍为 0（`src/` 只有骨架、CLI、配置、wire）；spike 代码只在 `spikes/`，不要直接搬进 `src/`。
