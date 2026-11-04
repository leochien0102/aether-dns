# M0 spikes (2026-11-04)

只用于 M0 风险验证的最小原型与证据；**不是生产代码**，不进入 `src/`，不修改 aether-proxy。

| 目录 | 内容 | 结论 |
|---|---|---|
| `m0-nftset/` | 纯 Rust/libc 的 nfnetlink 客户端：dump 成员、逐 IP 测试、批量添加；含 `run-spike.sh` 与 `evidence.log` | GO |
| `m0-parser/` | hickory-proto 0.25.2 解析/spike：合成语料、真实 wire 样本、边界包；含薄适配扫描器原型 | GO（raw-forward + 薄适配） |
| `m0-fixtures/` | fixture harness：22 份 `expected/chinadns.conf`、netns+mountns、假上游、baseline/coverage/wire 样本 | GO |

完整结论、证据、失败方案与 go/no-go 见 [`../M0-REPORT.md`](../M0-REPORT.md)。

## 复现（WSL2 / root）

```bash
# 依赖：rustc/cargo、nftables、python3、chinadns-ng 2025.08.09 x86_64 二进制
# 二进制固定 SHA256 842ea4e9...（见 M0-REPORT）

cd spikes/m0-nftset && ./run-spike.sh            # 自动建临时 netns，退出即删
cd spikes/m0-parser && ./run-spike.sh            # 生成 evidence-*.log
cd spikes/m0-fixtures && ./run-harness.sh all    # coverage + startup + behavior + probe
```

`m0-fixtures` 需要 root：`run-harness.sh` 会建 `aether-m0` netns、给 lo 加假上游地址、
在 mount namespace 里用 tmpfs 覆盖 `/etc/aether-proxy`、`/var/run/aether-proxy`、
`/usr/share/aether-proxy`，因此不会污染宿主文件，也不会使用真实 DNS。

## 证据文件

- `m0-nftset/evidence.log`：netlink dump/test/add/EEXIST/重试/失败模式/性能。
- `m0-parser/evidence-corpus.log`、`evidence-wire.log`、`evidence-edge.log`：hickory 解析与重编码差异。
- `m0-fixtures/coverage.json`：22 份配置的指令覆盖。
- `m0-fixtures/startup.json`：22/22 启动。
- `m0-fixtures/baseline.json`：9 个场景 / 29 次查询的基线（上游命中、客户端响应、nftset 内容）。
- `m0-fixtures/wire-samples.json`：chinadns-ng 实际发出的上游查询与返回给客户端的 38 个报文。
- `m0-fixtures/probe.json`、`edge-wire-samples.json`：畸形/边界查询对照。
