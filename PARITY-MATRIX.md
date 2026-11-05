# chinadns-ng 功能/行为对照矩阵（draft，2026-11-04）

> 状态：**第一版草稿**。已完成上游 README/`--help` 与 helloworld/SSR+ Makefile 的初步核对；逐项精确行为仍待补。
> 基线：chinadns-ng 2.0（最近提交 2025-08-09）。规则：默认 `match`；任何 `improve` / `intentionally differ` 必须在 `IMPROVEMENTS.md` 登记并经所有者批准。
> 证据：`[CN]` = [chinadns-ng README](https://raw.githubusercontent.com/zfl9/chinadns-ng/master/README.md) / [repo](https://github.com/zfl9/chinadns-ng)；`[HELP]` = README 内 `--help`；`[HW]` = [SSR+ Makefile](https://raw.githubusercontent.com/fw876/helloworld/master/luci-app-ssr-plus/Makefile)。

| ID | 功能/行为 | chinadns-ng 基线 | 证据 | aether-dns 决策 | 理由/备注 | 批准 | 验收方式 | 状态 |
|---|---|---|---|---|---|---|---|---|
| P-01 | CLI 与配置文件 | `-C/--config`，配置文件用长选项无 `--`；短/长选项、`--help`/`--version` | [HELP] | match（精确兼容待细化） | 先按原选项集合；是否保留全部短选项待定 | A 目标 | 选项/配置文件对照 + CLI 兼容测试 | draft |
| P-02 | 监听与传输 | `-b/--bind-addr`、`-l/--bind-port <port[@proto]>`；UDP/TCP；EDNS | [HELP][CN] | match | 端口/协议语法、错误处理待细化 | A 目标 | 端口/协议矩阵 + 报文测试 | draft |
| P-03 | 双组与自定义组 | `china`/`trust` + 最多 6 个自定义组；`--group`、`--group-dnl`、`--group-upstream`、`--group-ipset` | [CN][HELP] | match | 组数、默认 tag、组间竞争规则待细化 | A 目标 | 组配置/路由对照测试 | draft |
| P-04 | 域名列表与 tag | `chnlist-file`/`gfwlist-file`、`chnlist-first`、`default-tag`；tag:chn/gfw/none | [CN][HELP] | match | 列表格式、优先级、大小写/后缀规则待细化 | A 目标 | 列表/优先级 golden 测试 | draft |
| P-05 | tag:none IP 测试 | `ipset-name4/6`（支持 `family@table@set` nftset 形式）；`noip-as-chnip` | [CN][HELP] | match | IP 测试/回退语义、保留地址处理待细化 | A 目标 | 分类结果 + 过滤测试 | draft |
| P-06 | ipset/nftset 写入 | `add-tagchn-ip`、`add-taggfw-ip`、`group-ipset`、`no-ipset-blacklist` | [CN][HELP] | match（细节 pending） | netlink/nft 失败策略、屏障/重建同步待细化 | A 目标 | 集合写入/失败/重建测试 | draft |
| P-07 | no-ipv6 / AAAA | `-N/--no-ipv6 [rules]`：tag:<name>[@ip:*]、ip:china、ip:non_china；无规则=全部 AAAA 过滤 | [CN][HELP] | match（pending） | 规则组合/边界待细化 | A 目标 | AAAA 规则矩阵 | draft |
| P-08 | qtype 过滤 | `--filter-qtype <qtypes>`（u16） | [HELP] | match | 支持范围/错误处理待细化 | A 目标 | qtype 过滤测试 | draft |
| P-09 | DNS cache | `--cache`、`--cache-stale`、`--cache-refresh`、`--cache-nodata-ttl`、`--cache-min-ttl`、`--cache-max-ttl`、`--cache-ignore`、`--cache-db` | [CN][HELP] | match（pending） | TTL/负缓存/持久化格式待细化 | A 目标 | 缓存/过期/DB 对照测试 | draft |
| P-10 | verdict cache | `--verdict-cache`、`--verdict-cache-db`（tag:none 判定缓存） | [CN][HELP] | match（pending） | 键/过期/持久化待细化 | A 目标 | verdict 复用/防泄漏测试 | draft |
| P-11 | hosts / 本地 RR | `--hosts [path]`、`--dns-rr-ip <names>=<ips>` | [CN][HELP] | match | 格式/优先级/冲突待细化 | A 目标 | 本地记录测试 | draft |
| P-12 | DoT 与证书 | `--cert-verify`、`--ca-certs`；wolfssl 构建；上游可 `tls://` | [CN][HELP] | match（实现细节 pending） | Rust 侧可用 rustls；行为/证书校验要一致 | A 目标 | DoT 连接/校验/失败测试 | draft |
| P-13 | 上游格式/超时/重复 | 多上游、`udp://`/`tcp://`/`tls://`、`-o/--timeout-sec`、`-p/--repeat-times` | [CN][HELP] | match | 地址解析/并发/重试语义待细化 | A 目标 | 上游矩阵 + 超时/重复测试 | draft |
| P-14 | 运行模式/杂项 | `-n/--noip-as-chnip`、`-f/--fair-mode`、`-r/--reuse-port`、`-v/--verbose` | [HELP] | match | 选项语义/日志格式待细化 | A 目标 | 行为/日志对照测试 | draft |
| P-15 | 信号/退出/服务 | 信号收尾、退出码、OpenWrt 服务/启动脚本 | [CN] + 包 | pending | 需要看 OpenWrt 包与运行方式 | A 目标 | 信号/退出/服务测试 | draft |
| P-16 | helloworld/SSR+ 集成 | chinadns-ng 是 SSR+ 的**可选** include；另有 mosdns/dnsproxy/dns2socks 等 | [HW] | 上下文，不是行为 | 不把“可选”推断成缺陷；原因待查 | A 目标 | 集成说明/包 | draft |
| P-17 | 构建/工具链 | 上游 Zig 0.10.1 + C；DoT 可选 wolfssl | [CN] | 项目理由：Rust 重写 | 这是项目级理由，不是功能差异 | A 目标 | 可复现构建 | draft |
| P-18 | IPv6 DNS（AAAA/上游/ipset6/nftset） | README：完整 IPv4/IPv6、EDNS；`ipset-name4/6`、`family@table@set`、`no-ipv6`/`filter-qtype` | [CN][HELP] | match | 只做 DNS 解析/分类；不实现 IPv6 转发/代理 | A 目标 | IPv6 DNS/AAAA/ipset6 测试 | draft |
| P-19 | IPv6 转发/代理 | chinadns-ng 不是代理，无此功能 | [CN] | unsupported / out of scope | 属于 aether-proxy 未来数据面；aether-dns 不实现 | A 目标 | 不在 DNS 组件验收 | draft |

## P0：aether-proxy 当前 profile（第一交付，不是全量 chinadns-ng）

> 第一交付只覆盖 aether-proxy 当前 renderer 实际生成的子集；golden 用例来自 `aether-proxy/tests/fixtures/*/expected/chinadns.conf`。
> 全量 CLI/缓存 DB/hosts/DoT/高级 no-ipv6 等属于 P1；P0 先做“aether-proxy 可替换”的最小闭环。

| ID | P0 行为 | 证据 | 决策 | 验收 |
|---|---|---|---|---|
| P0-01 | `-C <config>` 启动 + 配置文件行格式 | aether-proxy `daemon.uc` 调用 `chinadns-ng ... -C`; fixtures | match | 用 aether-proxy 现有 expected/chinadns.conf 逐字节对照 |
| P0-02 | `bind-addr 127.0.0.1` / `bind-port 31053` | fixtures | match | 同上 |
| P0-03 | `china-dns` / `trust-dns`：T1/T2/T3、`tcp://`/`udp://`、`host@ip#port` | fixtures; `trust_dns_parts` | match | 上游格式/排序/失败语义对照 |
| P0-04 | `gfwlist-file` / `chnlist-file` / `default-tag`（chn/none/gfw） | fixtures | match | 模式 × 列表 golden |
| P0-05 | `ipset-name4` nftset；`ipset-name6 null`；tag:none IP 测试 | fixtures | match | chnroute4 判定测试 |
| P0-06 | `add-taggfw-ip <set>,null`（gfw4） | fixtures | match | 集合写入/失败测试 |
| P0-07 | 自定义组 `uproxy`/`udirect`/`boot`；`group-dnl`/`group-upstream`/`group-ipset` | fixtures | match | 组优先级/写入测试 |
| P0-08 | `no-ipv6`；`filter-qtype 65`（按配置条件） | fixtures | match | AAAA/HTTPS 过滤测试 |
| P0-09 | `cache 4096` / `cache-stale 3600` / `verdict-cache 4096` | fixtures | match | 缓存/stale/verdict 对照 |
| P0-10 | nftset 语法 `inet@aetherproxy@<set>` | fixtures | match（netlink 已验证） | 与 nft 渲染器联调 |
| P0-11 | 退出/信号/重启；daemon 子进程生命周期 | aether-proxy daemon | match | 启停/崩溃/重载测试 |
| P0-12 | `tls://` 上游（仅当配置使用；否则 P1） | fixtures 含 tls 用例 | conditional | 先支持 tcp/udp；tls 作为 P0-conditional 或 P1 |

M0 证据（2026-11-04，详见 [M0-REPORT.md](M0-REPORT.md)）：
- P0-01/02：22/22 `expected/chinadns.conf` 在真实 chinadns-ng 下启动（`startup.json`）。
- P0-03/04/07/08/09：harness 9 场景/29 查询，13/13 sanity（`baseline.json`）。
- P0-05/06/10：nftset spike 的 dump/test/add/EEXIST/重试 + harness 的 `gfw4/uproxy4/udirect4` 断言。
- P0-11：**未验证**（M0 不做信号/重启）；P0-12：22 份 fixture 均未使用 `tls://`，仍未验证。

**M2 状态（2026-11-05）**：P0-01/02 22/22 启动；P0-03/04/05/06/07/08/09/10 在 M0 固定
9 场景/29 查询 + 13/13 sanity 上 `diff=0`（`tests/acceptance_baseline.sh`，unprivileged netns）。
这只覆盖固定 baseline，不等于全量 chinadns-ng parity；M2-05 单侧失败回退、M2-08 仅正向 A/AAAA、
IPv4 set、verdict key/淘汰等 known gap 见 [M2-REPORT.md](M2-REPORT.md) §5。
P0-11 信号/退出已实现，重启/服务脚本未真机验证；P0-12 22 份 fixture 未用 `tls://`，DoT 未实现。

**M1 历史（2026-11-05）**：M1 只做可跑骨架，当时上表 P0 行仍为 `draft`；组/列表/tag/
`tag:none` IP 测试/nftset/cache/`no-ipv6`/`filter-qtype`/DoT 均未实现，详见
[M1-REPORT.md](M1-REPORT.md) §5。M2 已完成，见上。

P0 不做：完整短选项集合、`cache-db`/`cache-refresh`、`hosts`/`dns-rr-ip`、`noip-as-chnip`/`fair-mode`/`reuse-port`、完整 6 自定义组、独立 DoT 全量、IPv6 代理。以上归 P1/P2，除非 aether-proxy 实际配置用到。

## 完成标准

- 每一行有“基线证据 + 决策 + 验收方式”；
- 所有 `improve` / `differ` 有 `IMPROVEMENTS.md` 记录和所有者批准；
- 矩阵覆盖 helloworld 当前实际用法，而不只是 chinadns-ng 全量文档。

## 待补

- `-C/--config` 的完整语法与非法输入行为；
- `--group*` 2.0 的组数/默认值/冲突规则；
- `no-ipv6` 规则组合、`filter-qtype` 精确语义；
- cache/verdict 的 TTL、DB 格式、并发/负缓存；
- nftset 地址格式、写入失败/屏障/重建；
- DoT 证书校验细节；
- OpenWrt/SSR+ 的实际调用方式与“可选”原因。
