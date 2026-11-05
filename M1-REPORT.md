# aether-dns M1 报告：可跑骨架

> 日期：2026-11-05。范围：**只做 M1 骨架**，不进入 P0 行为等价、不进入 M2。
> 结论：M1 的 10 张卡全部复审合并；**22/22 配置可启动、6/6 转发矩阵通过、49 个测试全过**。
> 这不是 P0 drop-in；组/列表/tag/nftset/cache/过滤仍未实现，M1 不声明 parity。

## 0. 结论摘要

| 验收 | 结果 |
|---|---|
| `cargo test --all` / `tests/review.sh` | 49 个测试，0 failed，`FAIL=0` |
| 22 份 `expected/chinadns.conf` 启动 | `22/22 configs stayed up` |
| A/AAAA/HTTPS × UDP/TCP 转发矩阵 | `6/6 PASS` |
| `-V` 版本契约 | `aether-dns 2026.11.04`；模拟 aether-proxy `parse_version` 通过最小版本 |
| 信号/退出 | SIGTERM/SIGINT 500 ms 内排空并退出 0；第二次信号立即退出 1 |

当前 M1 代码合并最后为 `913519f`；M1 收口报告与状态提交在其后。M1 结束即停，等所有者确认后再进 M2。

## 1. 范围与不做

M1 只证明：**配置能读、端口能听、报文能转发、进程能收尾**。

- 启动只读 `-C`；所有查询发给**第一个 `trust-dns`**。
- 不做组/列表/tag、`tag:none` IP 测试、nftset、cache/verdict、`no-ipv6`、`filter-qtype`、DoT。
- 不修改 aether-proxy，不启动真实服务，不做真机/长稳。
- 不把 M1 的“纯转发到第一个 trust”写成 parity `match`。

## 2. 任务卡与提交

| 卡 | 内容 | 合并提交 | 分支提交 |
|---|---|---|---|
| T1.0 | 仓库/构建骨架 | `bc6c16d` | main |
| T1.1 | worktree 脚本 | `26c8f6e` | main |
| T1.2 | 配置解析（22 份 fixture） | `a1cdc25` | `17e8d19` |
| T1.3 | DNS wire 工具（raw-forward） | `cacf4db` | `ccf65fc` + `bb91040` |
| T1.4 | UDP 单上游转发 | `67b3422` | `3425b39` |
| T1.5 | TCP 客户端/上游 | `44aa7b3` | `51d81d5` |
| T1.6 | CLI 版本/帮助 | `938f0cc` | `fa1eb03` |
| T1.7 | 信号/退出（500 ms） | `65132ba` | `dc873d7` |
| T1.8 | 22/22 启动验收 | `3887a52` | `a55334f` |
| T1.9 | 6/6 转发矩阵 | `913519f` | `2bf4750` |

所有卡都经过：发卡前检查 → 执行者提交报告 → 设计负责人跑验收/回归 → `--no-ff` 合并并移除报告 →
删除 worktree/分支 → main 上全量测试。

## 3. 实现概况

- 单 crate、单二进制 `aether-dns`，edition 2024。
- `src/config.rs`：19 条指令、22 份 fixture、fail-closed、重复语义（追加/最后生效/并集）。
- `src/dns/mod.rs`：hickory 0.25.2 解析；字节偏移扫描与原地改写（ID/RA/TC/additional/NODATA）；
  转发路径不重编码。
- `src/server.rs` / `src/upstream.rs`：Tokio current-thread；UDP/TCP 客户端；每监听器一个任务；
  第一个 trust；显式 `udp://`/`tcp://` 优先，否则跟随客户端协议；5 s 超时、单次尝试；
  响应恢复 ID、RA=1、条件性裁 additional；失败合成 SERVFAIL。
- `src/main.rs`：`-C`、`-V`/`--version`、`--help`；TERM/INT 首次信号 500 ms 排空、退出 0，
  第二次信号立即退出 1。
- 测试：3 CLI、21 配置、6 wire、6 lifecycle、4 TCP、9 UDP = 49。

`-V` 首行 `aether-dns 2026.11.04`；按 aether-proxy `parse_version` 的首行/第二字段规则，
`2026.11.04 >= 2025.08.09`。尚未在真实 aether-proxy lab 里做替换（M3）。

## 4. 验收命令

```bash
cargo test --all
bash tests/review.sh
bash tests/acceptance_startup.sh
bash tests/acceptance_forward.sh
```

最近一次在 main 上的结果：

- `tests/review.sh`：3+21+6+6+4+9=49 passed，`FAIL=0`。
- `tests/acceptance_startup.sh`：22 份配置逐份启动 0.3 s，`22/22 configs stayed up`。
- `tests/acceptance_forward.sh`：6 行矩阵全 PASS，`6/6 PASS`。
- 两个验收脚本都不需要 root/netns/nft；fixtures 只读，可用 `AETHER_PROXY_FIXTURES` 覆盖路径。

## 5. M1 已知差距（M2 输入）

1. **query TC 未清**：M1 只改 query 的 ID；chinadns-ng 还会清 TC。P0 客户端不发 TC，M2 补 `clear_tc`。
2. **畸形 query 丢弃**：`check_query` 失败的客户端报文直接丢弃；chinadns-ng 回“坏响应”。M2 按基线决定。
3. **响应 question 未比对**：`valid_reply` 只校验 ID/QR/qdcount；M2 加 question 匹配。
4. **默认 bind**：未写 `bind-*` 时 M1 用 127.0.0.1:53；chinadns-ng 默认 65353。P0 fixture 都显式设置。
5. **P0 路由/写入/过滤全缺**：组、列表、tag、`tag:none` IP 测试、nftset、cache/verdict、
   `no-ipv6`、`filter-qtype`、DoT 均未实现；M1 不声明 P0 parity。
6. **TCP 客户端无读超时**：连接后不发送/半帧会占住任务；停机时会被关闭，M2 加超时/并发上限。
7. **上游只取第一个 trust**：无多上游竞争/重试；M2 按 chinadns-ng 语义补。
8. **T1.2 配置测试路径**：`tests/config.rs` 仍用绝对路径读 aether-proxy fixtures；本工作区可复现，
   跨机器/CI 前应改用 `tests/data/config` 副本或 `AETHER_PROXY_FIXTURES`。
9. **未做 aether-proxy lab 替换/真机**：`-V` 只用模拟解析验证；M3 做真实替换与回退。
10. **TLS 未实现**：`tls://` 启动报错；P0-conditional，22 份 fixture 均未使用。

## 6. 下一步（不自动开工）

建议 M2 按以下顺序，逐项对 M0 的 `baseline.json` 做 diff，不并行第二个 DNS 引擎：

1. 组/列表/tag 与 `default-tag` 路由（先让 P0-03/04/07 对齐）。
2. `tag:none` IP 测试 + nftset 写入（IMP-004；先测后加、ACK、EEXIST 重试）。
3. `no-ipv6` / `filter-qtype` 过滤（P0-08）。
4. cache/stale/verdict（P0-09，TTL 原地递减）。
5. TCP 客户端读超时、响应 question 匹配、query TC 清理。
6. M2 收口：9 场景/29 查询 baseline diff + M0 的 13/13 sanity。

M2 开始前需要所有者确认范围；M1 不自动进入 M2。
