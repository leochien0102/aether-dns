# aether-dns M2 报告：P0 行为对齐

> 日期：2026-11-05。范围：**M2 P0**（aether-proxy 当前 profile），不进入 P1/M3。
> 结论：M2 的 12 张卡全部复审合并；**M0 baseline 9 场景/29 查询与 13/13 sanity 差分 0**；
> 210 个 Rust 测试、22/22 启动、6/6 传输矩阵全过。M2 完成即停，等所有者确认 M3。

## 0. 结论摘要

| 验收 | 结果 |
|---|---|
| `tests/acceptance_baseline.sh` | `29/29 queries`、`13/13 sanity`、`diff=0`（unprivileged netns） |
| `cargo test --all` / `tests/review.sh` | 210 passed，`FAIL=0` |
| 22 份 `expected/chinadns.conf` 启动 | `22/22` |
| A/AAAA/HTTPS × UDP/TCP 传输 | `6/6` |
| P0-05/06/10 nftset | 成员测试、test-then-add、gfw4/uproxy4/udirect4 写入经 baseline 对齐 |

代码合并最后为 `c5c72e7`；收口文档提交在其后。M2 结束即停，不自动进入 M3/P1。

## 1. 范围与不做

M2 只覆盖 aether-proxy 当前 renderer 生成的 P0 子集：

- 配置 22 份、列表/tag、组/上游并发、路由、`tag:none` IP 测试、nftset 写入、过滤、
  正向 A/AAAA cache/stale、verdict cache、query TC、TCP 帧读超时。
- 不做 P1：hosts/local RR、cache-db/refresh/min-max TTL、完整 CLI、`noip-as-chnip`、
  `fair-mode`/`reuse-port`、6 自定义组全量、DoT、IPv6 IP 测试/写入。
- 不改 aether-proxy；不声称全量 chinadns-ng parity，只声称 M0 固定 9/29 + 13 sanity 对齐。

## 2. 任务卡与提交

| 卡 | 内容 | 合并提交 | 分支提交 |
|---|---|---|---|
| M2-01 | 列表/tag/组优先级 | `e05bf8f` | `a8d0d87` |
| M2-02 | 上游组并发 | `14298cc` | `c415486` |
| M2-03 | nftset netlink | `da4c67d` | `c10bdc2` |
| M2-04 | 按 tag/组路由 | `5a45c10` | `822088e` |
| M2-05 | tag:none 双查询/IP 测试 | `c0d7c20` | `8195fe0` |
| M2-06 | nftset 写入 | `5193c60` | `cdb2371` |
| M2-07 | no-ipv6 / filter-qtype | `436d3c3` | `1a690ca` |
| M2-08 | cache/stale | `83c002b` | `074a684` |
| M2-09 | verdict cache | `06e9180` | `e74bc29` |
| M2-10 | query TC 清位 | `d46e7a2` | `6bc2c83` |
| M2-11 | TCP 帧读超时 | `d64627c` | `5ea362a` |
| M2-12 | P0 baseline 差分 | `c5c72e7` | `1d37f65` |

测试基础设施加固：`da17354`、`5ea362a`、`702c93f`（UDP+TCP 同端口预留重试）。

## 3. 实现概况

- `lists.rs`：wire domain 后缀、label 边界、8 层截断、自定义组后声明优先、gfw 先于 chn；
  `Router::load` 启动一次加载。
- `upstream_group.rs`：组内多上游并发、首个合法响应、其余取消。
- `nftset.rs`：纯 libc netlink；`inet@table@set`、GETSETELEM/NEWSETELEM、interval、
  test-then-add、ACK、EEXIST 重试；不缓存 handle。
- `server.rs`：路由/`tag:none` 双查询与 IP 测试、writer、过滤、cache、verdict、TC 清位、
  TCP 5 s 帧读超时；保留 M1 的 ID/RA/additional/SERVFAIL 与 500 ms drain。
- `cache.rs`：正向 A/AAAA LRU；TTL 原地递减、stale TTL0、容量淘汰、命中 writer 一次。
- `verdict.rs`：wire qname → bool；容量任意淘汰；无 TTL/DB。
- 测试：210 个 Rust 测试；baseline 差分器掩码 ID/压缩/hit 顺序，严格比较 flags/TTL/answers/
  upstream 多重集/sets/sanity。

## 4. 验收命令与证据

```bash
bash tests/acceptance_baseline.sh
cargo test --all
bash tests/review.sh
bash tests/acceptance_startup.sh
bash tests/acceptance_forward.sh
```

最近一次在 main 上：

- baseline：`29/29 queries`、`13/13 sanity`、`diff=0`；不需要 sudo，使用 `unshare -Urnm`。
- Rust：210 passed，`FAIL=0`。
- startup：22/22；forward：6/6。

baseline 覆盖 P0-01/02 启动、P0-03/04/07/08/09 路由/列表/组/过滤/缓存、P0-05/06/10 nftset；
逐条结果见 `M2-12` 报告和 `spikes/m0-fixtures/baseline.json`。

## 5. M2 已知差距（M3/P1 输入）

1. **M2-05 单侧失败回退**：china/trust 一侧失败、超时或空组时使用另一侧；chinadns-ng 部分路径
   会等待到超时丢包。**不是 parity**，正常 baseline 不触发。
2. **M2-05 空 trust / IPv6**：空 trust 直接采用 china、不做 IP 测试；AAAA 在 `ipset-name6=null`
   时 non-china；不做 IPv6 IP 测试。
3. **M2-08 仅正向 A/AAAA**：不缓存 NODATA/NXDOMAIN、SERVFAIL、TC、仅 TXT/CNAME/HTTPS；
   无负缓存、cache-db、后台 refresh、TTL min/max。
4. **M2-09 verdict**：key 只含 wire qname；容量满任意淘汰；无 TTL/DB；AAAA null 缓存 false。
5. **nftset 仅 IPv4**：v6 语法可解析但不写入；P0 `ipset-name6` 都是 null。
6. **response question 匹配**：chinadns-ng 也不比对；不是 parity 缺口，防御性匹配留 M3 安全项。
7. **TCP 读超时固定 5 s**：无连接数上限、不可配置；防御性，不是 P0 parity。
8. **默认 bind/TLS**：未写 `bind-*` 仍用 127.0.0.1:53（P0 fixture 全显式）；`tls://` 未实现。
9. **测试 fixture 路径**：`tests/config.rs` 仍读 aether-proxy 绝对路径；跨机器/CI 前补副本或 env。
10. **无 aether-proxy lab/真机**：baseline 是隔离 namespace，不是真实路由器；M3 做替换/回退。
11. **baseline 是固定 9/29**：不等于全量 chinadns-ng parity；P1 输入需另测。

## 6. 下一步（不自动开工）

M3 建议顺序（需所有者确认）：

1. aether-proxy lab 真替换：`-V`、启动、停止、重启、回退；真实 `/etc`/nft 表。
2. DoT（仅当 P0 fixture/真实配置使用）；证书校验与失败语义。
3. P1：hosts/local RR、cache-db/refresh/min-max、noip-as-chnip/fair-mode/reuse-port、
   完整 6 组、IPv6 set 写入。
4. 安全/健壮性：response question 防御性匹配、TCP 连接上限、fuzz/畸形报文、长稳。
5. 把 M2-05/M2-08/M2-09 的 known gap 逐项决定：修成 parity 还是登记 improve。

**M2 完成即停；等所有者确认 M3 范围后再拆卡。**
