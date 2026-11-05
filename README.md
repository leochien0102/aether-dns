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
- [spikes/](spikes/)：M0 最小原型与可复跑证据（非生产代码）。
- [docs/tasks/](docs/tasks/README.md)：任务卡与执行约定（M1 全部完成；M2 第一波进行中）。

旧项目已停止并归档：

- 完整树：`../archive/aether-dns-2026-11-04/`
- 便携包：`../archive/aether-dns-2026-11-04-src.tar.gz`
- 停止记录：`../archive/aether-dns-2026-11-04/STOP.md`

当前阶段：**M2 完成（2026-11-05）**：12/12 卡合并；210 测试、22/22 启动、6/6 转发、
baseline 29/29 + 13/13 sanity、diff=0。**M2 结束即停**，等所有者确认后再进 M3/P1。
M2 只覆盖 M0 固定 9/29，不是全量 chinadns-ng parity。
