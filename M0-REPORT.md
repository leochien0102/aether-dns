# aether-dns M0 风险 spike 报告

> 日期：2026-11-04。范围：**只做 M0**（3–5 天时间盒），0 生产代码、0 生产配置改动、未修改 aether-proxy。
> 结论：三个 spike 全部 **GO**，未触发停止条件；建议在所有者确认后进入 M1。
> 本报告的证据全部来自 `spikes/` 下的可复跑原型与本机隔离环境；不是文档推断。

## 0. 结论摘要

| Spike | 结论 | 关键证据 | 是否阻塞 M1 |
|---|---|---|---|
| nftset（Rust netlink） | **GO**：可靠、快、无 libnftnl 依赖；语义已与 chinadns-ng 对齐 | `spikes/m0-nftset/evidence.log` | 否 |
| parser（hickory-proto） | **GO（薄适配）**：hickory 只做解析/检查；转发必须 raw bytes + 字节偏移适配，禁止重编码 | `spikes/m0-parser/evidence-*.log` | 否 |
| fixture harness | **GO**：22/22 配置可启动，19 条指令全覆盖，13/13 行为 sanity 通过，已有可 diff 基线 | `spikes/m0-fixtures/*.json` | 否 |

**没有出现“nftset 无可靠 netlink 方案”的情况，因此不 stop。**
M0 结束即停，不自动进入 M1；等待所有者确认（见 §7）。

环境：WSL2 Ubuntu 26.04，kernel `6.18.54.1-microsoft-standard-WSL2`，rustc/cargo `1.93.1`，
nft `1.1.6`，hickory-proto `0.25.2`；基线二进制为 chinadns-ng `2025.08.09`（`23af4b1`，
wolfssl-5.8.2，x86_64-linux-musl），SHA256
`842ea4e9816efd91d39bc76ead5c4a42e79011757e37c521b4270b675cfcb30c`（与 aether-proxy
`tests/ci/chinadns.sh` 的 pin 一致）。

---

## 1. 范围与做法

只做 M0 的三件事，且只写 spike：

1. **nftset**：Rust 直接 nfnetlink 查询 `inet@aetherproxy@chnroute4` 成员、向 `gfw4`/`uproxy4`
   添加元素；在 netns 中验证；不用 `nft` CLI 实现。
2. **parser**：用 hickory-proto 解析 aether-proxy profile 会遇到的查询/响应；与 chinadns-ng
   的真实行为比较；决定是否需要薄适配。
3. **fixture harness**：把 `aether-proxy/tests/fixtures/*/expected/chinadns.conf` 变成可重复的
   配置/行为入口；假上游；记录覆盖。

约束执行情况：

- 没有新增 `aether-dns/src/`；所有代码在 `spikes/`，可整体删除。
- 没有修改 aether-proxy；只读取其 renderer、fixtures 和 CI 脚本。
- 没有启动真实服务/真实上游：所有运行在一次性 netns 内，假上游绑定在 netns 的 lo 别名上。
- 没有做 whole-unit/cgroup/fuzz/30 分钟长稳；没有扩到 P1/P2。
- 设计缺口登记到 `IMPROVEMENTS.md`（IMP-004/005/006），没有“顺手改”。

---

## 2. Spike 1：nftset / nfnetlink

### 2.1 目标集合的形状（来自 aether-proxy 实际 renderer）

`aether-proxy` 仓库内的 `aether-proxy/src/lib/render/nft.uc` 与 fixtures 的 `expected/aether-proxy.nft` 显示，P0 用到的集合是：

```nft
set chnroute4 { type ipv4_addr; flags interval; auto-merge; elements = { ... }; }
set gfw4     { type ipv4_addr; flags interval; auto-merge; }
set uproxy4  { type ipv4_addr; flags interval; auto-merge; }
set udirect4 { type ipv4_addr; flags interval; auto-merge; }
```

没有 `timeout`/`dynamic`/`counter`；`ipset-name6 null`，P0 只写 IPv4。chinadns-ng 的 set 名语法
为 `family@table@set`，与 renderer 的 `nft_set()` 一致。

### 2.2 原型

`spikes/m0-nftset/`：约 700 行，依赖只有 `libc`。直接 `socket(AF_NETLINK, SOCK_RAW,
NETLINK_NETFILTER)`，手写 `nlmsghdr`/`nfgenmsg`/`nla`：

- **成员测试**：`NFT_MSG_GETSETELEM`（子系统的 10，消息 13），一次一个元素，key 填 IP；
  返回 `NFT_MSG_NEWSETELEM` = 命中，`NLMSG_ERROR -ENOENT` = 未命中。
- **添加**：`NFNL_MSG_BATCH_BEGIN` + `NFT_MSG_NEWSETELEM` + `NFNL_MSG_BATCH_END`，一次
  `send`；每个 IP 写 interval begin（key=IP）+ interval end（flag=1，key=IP+1）；带
  `NLM_F_REQUEST|NLM_F_CREATE|NLM_F_ACK`，读一个 ACK。
- **dump 成员**：`NFT_MSG_GETSETELEM + NLM_F_DUMP`，解析 begin/end 端点，排序配对。
- 一个进程一个 socket、不缓存 set handle；每个操作按名字解析，所以 aether-proxy 整表重建后自动恢复。

### 2.3 结果

来自 [`spikes/m0-nftset/evidence.log`](spikes/m0-nftset/evidence.log)：

| 场景 | 结果 |
|---|---|
| dump `chnroute4` | 正确还原 `1.0.1.0-1.0.3.255`、`223.255.252.0-223.255.253.255`（nft 声明时已 merge） |
| test 命中 / 未命中 | `FOUND` / `NOT_FOUND errno=-2`，单次 ~5–40 µs（含首次建 socket） |
| bench test ×1000 | 5.46 ms，**5.5 µs/次** |
| add 3 个 IP（一个 batch） | `ACK_OK` ~55–90 µs |
| bench add 500（50 batch） | 2.77 ms，**5.5 µs/IP** |
| 已存在区间内的 IP | `ERROR errno=-17 (EEXIST)`，**整批回滚**（同批的新 IP 未写入） |
| 精确重复的单个 IP | `ACK_OK`（不加 `NLM_F_EXCL` 时幂等） |
| 先 test 再 add（`add-safe`） | 2 个 IP 120 µs；区间内的跳过，只加缺失的 |
| set 不存在 | `-ENOENT`；按“不在集合”处理即可 |
| 无 `CAP_NET_ADMIN` | `-EPERM`；按“不在集合”处理并记日志 |
| 运行中删除再重建 table | 先 `-ENOENT`，整表重建后同一进程下一次 add `ACK_OK`（无 handle 缓存） |
| IPv6 集合 | 同一路径可用（测试/添加/dump 均通过） |

对照：`nft` CLI 逐条添加 50 次耗时 **1.187 s（~23.7 ms/次）**，比 netlink 慢约 3 个数量级。
`ldd` 显示原型只依赖 libc/libgcc，无 libnftnl/libmnl。

### 2.4 关键发现：test-then-add 是必需的

在 interval + auto-merge 集合里：

- 精确重复的元素是幂等的；
- 但**添加一个已落在现有区间内部的 IP**（例如集合已有 `1.2.3.0/24`，再 add `1.2.3.77`）会
  返回 `-EEXIST`，而 nfnetlink batch 是原子的：**整批失败，其他新 IP 也不会写入**。

这解释了 chinadns-ng 为什么先 `GETSETELEM` 测试、再批量添加缺失项（`src/ipset.c`
`end_add_ip_nftset` → `test_ips`）。aether-dns 必须照做；我们的 `add-safe` 原型已实现
“逐 IP 测试 → 批量加缺失 → 遇到 `-EEXIST` 重试”。并发写者插入覆盖区间时，重试路径已验证
（人为拉大 test→add 窗口，日志出现 `race (errno=-EEXIST), retrying`，最终集合正确）。

### 2.5 `nft` CLI 互操作（既有行为，不是回归）

直接 netlink 写入的 interval 不会被 nft 用户态“auto-merge”。因此在一组**相邻单 IP**写入后，
集合里是多个未合并区间；此时 `nft add element` 对该集合可能报
`Could not process rule: No such file or directory`。我们用**真实 chinadns-ng 2025.08.09**
复现了完全相同的现象（同一个 set、同样 3 个元素、同样失败）。

影响评估：

- `nft list set` / `nft -j list set`、成员测试、删除/重建都正常；
- aether-proxy 的动态集合导出用 `nft -j list set`，reload 用整表 `nft -f` 重渲染
  （`aether-proxy/src/lib/apply.uc`、`reload.uc`），**不依赖 `nft add element`**；- 因此这不是 aether-dns 引入的回归，也不阻塞 M1；登记为 IMP-004 的已知约束。

### 2.6 被否方案

| 方案 | 为什么否 |
|---|---|
| 每次更新调 `nft` CLI | ~23.7 ms/次、进程开销、错误语义不可控；明确违反“不要用慢 nft 命令凑” |
| libnftnl/libmnl 绑定 | 能工作，但引入 C 依赖与 OpenWrt 交叉编译负担；纯 Rust 已足够 |
| 启动时 dump 全量成员并缓存 | 需要区间配对、reload 失效、并发失效；P0 只需要逐 IP 判定，与上游语义一致 |
| 无 ACK 的 fire-and-forget（上游做法） | 无法区分成功/失败；实测会等满 500 ms 收超时；对 aether-proxy 的路由集合不可接受 |
| 加 `NLM_F_EXCL` | 已存在元素直接 `-EEXIST`，整批回滚，更容易失败 |
| 精确重复直接 add、不 test | 精确重复幂等，但区间内 IP 会 `-EEXIST`；必须 test |

### 2.7 结论

**GO。** 纯 Rust netlink 是可靠方案：查询 5 µs、批量写 50–90 µs、失败模式可识别、reload 后自愈、
无 C 依赖。M1 直接移植该模块，按 IMP-004 的策略实现。

---

## 3. Spike 2：hickory-proto 解析与转发

### 3.1 目标

aether-proxy 的 profile 实际会产生：A/AAAA/HTTPS(65) 查询、EDNS0、UDP/TCP 客户端、来自
T1/T2/T3 上游的响应（常见压缩名、CNAME 链、NXDOMAIN/NODATA、可能的未知 RR）。需要回答：

1. hickory 能否解析这些报文？
2. 能否用 hickory 解析后重新编码再转发？
3. 不能的话，薄适配要做什么？

### 3.2 合成语料（20 例）

`spikes/m0-parser/evidence-corpus.log`：

- **17/20 解析成功**；3 个畸形包（11 字节、缺 question、label 长度 64）被拒——chinadns-ng
  的 `check_msg` 同样不接受它们（虽然它会回一个“坏包”响应，见 3.4）。
- **重编码只有 7/17 与原字节一致**：hickory 会重新压缩名字（`resp-A` 64→49 字节、
  `resp-cname+A` 109→79），并会丢掉尾部多余字节；两 question 的包重编码后
  header `qdcount=2` 但只写 1 个 question，产生畸形输出。
- 未知 RR（type 65280）能以 `RData::Unknown` 解析并在重编码后保留；HTTPS(65) 正常。
- EDNS/OPT：hickory 把 OPT 放在 `extensions()`，重编码仍保留；而 chinadns-ng 对“good”
  响应会把 additional 整段裁掉（见 3.3）。

### 3.3 chinadns-ng 的实际报文行为（源码 + 实测）

源码（`2025.08.09`）：

- 只解析 header + 一个 question，其余原样转发；查询/响应都要求 `qdcount == 1`。
- 转发上游时**重写 ID**（每个上游独立 ID），并清掉查询的 TC 位（`dns.c:check_msg`）。
- 响应：`dns_check_reply` → `rm_additional` 对 `NOERROR/NXDOMAIN && !TC` 的响应
  **删掉 additional 段**（`additional_count=0`，长度截断到 authority 末尾），并置 `RA=1`；
  其他 rcode/TC 响应原样保留。
- `no-ipv6` / `filter-qtype`：用 header + question 拼一个 NODATA 空响应（`dns_empty_reply`）。
- cache：保存“已裁 additional”的原始响应，命中时用 `dns.update_ttl` 原地递减各 RR 的 TTL
  （`CacheMsg.zig` / `dns.c`），不重新编码。

fixture harness 捕获的真实 wire（`spikes/m0-fixtures/wire-samples.json`）：

- **38/38 报文 hickory 解析成功**（上游查询 + 客户端响应；A/AAAA/HTTPS、TCP/UDP）。
- 上游查询 ID 与客户端 ID 不同（0x0100/0x0200… vs 0x4242），确认 ID 重写。
- 带 EDNS 的客户端查询：上游查询 `ar=1`（OPT 被转发），客户端响应 `ar=0`（OPT 被裁掉）。
- 已压缩的响应重编码恰好字节相同；但这只是“输入恰好已是最紧凑形式”，不能作为依赖。

### 3.4 边界包对照（8 例）

`spikes/m0-fixtures/probe.json` + `spikes/m0-parser/evidence-edge.log`：

| 输入 | chinadns-ng | hickory | 结论 |
|---|---|---|---|
| 正常 A | 转发，返回 A | 解析 | 一致 |
| 2 个 question | 不转发，直接回一个空/坏响应 | 解析（但重编码会丢第二个 question，header 仍写 qd=2） | **不能用 hickory 做转发门禁** |
| qdcount=0 | 不转发，回 12 字节空响应 | 解析（0 query） | 同上 |
| 尾部 4 字节垃圾 | 原样转发（上游字节完全一致，ID 除外） | 解析，重编码丢垃圾 | **raw forward 才能等价** |
| label 长度 64 | 不转发，回显一个坏响应 | 拒绝 | 双方都不接受为正常查询 |
| 11 字节 | 不转发，回 11 字节 | 拒绝 | 同上 |
| 查询带 TC 位 | 清 TC 后转发 | 解析 | 需要原地改 header |
| 混合大小写 qname | 原样保留并转发 | 原样保留 | 一致 |

### 3.5 薄适配的结论

**hickory 只做语义检查，绝不做转发编码。** 转发路径：

1. 收到客户端报文 → 用一个**故意宽松**的 header/question 检查（与 P0 实际客户端一致即可）
   + hickory 解析用于取 qname/qtype/EDNS；
2. 转发**原始字节**（只原地改 ID、清查询 TC）；
3. 收到上游响应 → hickory 解析取 answer IP/TTL/rcode 用于路由/缓存/nftset；
4. 回客户端前对**原始字节**原地：改 ID、置 RA、按需裁 additional、TTL 递减；需要 NODATA
   时用 header+question 拼空响应。

为此需要一个小型**字节偏移扫描器**（不是完整 parser、不是 strict9）：

- `skip_name`（含压缩指针，只跳过不跟随）、`skip_rr`、`cut_additional`（header+question+
  answer+authority 的结束偏移）；
- 已实现并在 20/20 合成包上验证（17 个正常 + 3 个预期拒绝），见
  `spikes/m0-parser/src/main.rs`；
- 同一扫描器也用于 cache 的 TTL 原地递减；这正是 chinadns-ng 的做法（`dns.update_ttl`），
  避免重编码。

**被否方案：**

| 方案 | 为什么否 |
|---|---|
| hickory 解析 → `to_vec()` → 转发 | 字节会变（压缩/丢尾部/多 question 畸形）；还会保留 chinadns-ng 会裁掉的 OPT |
| 手写完整 DNS parser（旧项目 strict9 方向） | 不需要；hickory 已覆盖正常路径，薄适配只做偏移 |
| 把 hickory 的接受/拒绝当作转发门禁 | 与 chinadns-ng 的宽松检查不一致（2 question、尾部垃圾）；会改变可见行为 |

### 3.6 已知边界差异（不阻塞 P0，登记为观察项）

- 2 question / qdcount=0 / 畸形 label：chinadns-ng 会回一个“坏响应”，aether-dns 若直接丢弃，
  客户端体验不同。P0 实际客户端不会发这些包；是否复刻留到 P1/IMP 评估。
- 超长/异常压缩指针：hickory 可能拒绝而 chinadns-ng 原样转发；P0 风险低。

### 3.7 结论

**GO（带薄适配）。** hickory-proto 0.25.2 足以覆盖 P0 的解析需求；转发必须 raw bytes；
薄适配是约 100–150 行的偏移扫描 + 原地 header/TTL 操作，不是新 DNS 引擎。M1 按此实现，
M2 用 harness 的 wire 样本回归。

---

## 4. Spike 3：fixture harness

### 4.1 做了什么

`spikes/m0-fixtures/harness.py`（Python，测试工具，非生产代码）：

1. 解析 22 份 `aether-proxy/tests/fixtures/*/expected/chinadns.conf`，记录指令/取值覆盖；
2. 在一次性 netns + mount namespace 中运行**真实 chinadns-ng**：
   - tmpfs 覆盖 `/etc/aether-proxy`、`/var/run/aether-proxy`、`/usr/share/aether-proxy`，
     保持 golden 配置**逐字节**读取；
   - 给 lo 加 `223.5.5.5`、`119.29.29.29`、`114.114.114.114`、`202.96.128.86`、
     `202.96.134.133` 等地址，假上游同时监听 UDP+TCP 的 53/31082/31083；
3. 假上游按 qname/qtype 返回确定性的 A/AAAA/HTTPS/NXDOMAIN/非中国 IP，并记录每次命中的
   端点/协议；
4. 记录客户端响应、上游命中、`gfw4/uproxy4/udirect4` 内容，输出 baseline。

### 4.2 覆盖

`coverage.json`：**22 份配置、19 条指令、missing=[]**。

| 指令 | 次数 | 指令 | 次数 |
|---|---|---|---|
| bind-addr / bind-port | 22 / 22 | group | 66 |
| china-dns / trust-dns | 22 / 22 | group-dnl / group-upstream | 66 / 66 |
| ipset-name4 / ipset-name6 | 22 / 22 | group-ipset | 44 |
| gfwlist-file | 21 | chnlist-file | 16 |
| default-tag | 22 | add-taggfw-ip | 10 |
| no-ipv6 | 21 | filter-qtype | 21 |
| cache / cache-stale / verdict-cache | 22 / 22 / 22 | | |

所有 renderer 可能生成的 P0 指令都至少出现一次；`t14-no-v6-filter` 是唯一不带
`no-ipv6`/`filter-qtype` 的配置，正好作为“不过滤”的对照。

### 4.3 启动

`startup.json`：**22/22** 配置在真实 chinadns-ng 下启动并保持存活 0.3 s（含 nft 集合、
T1/T2/T3、udp/tcp、多个 group、10 份带 `add-taggfw-ip` 的配置）。

### 4.4 行为基线

`baseline.json`：9 个场景、29 次查询、**13/13 sanity 通过**。代表性观察：

- **gfw 模式**（`t14-gfw-t1`）：`*.gfw.example` 只命中 trust（31082/31083，TCP），客户端拿到
  trust 的 A；`gfw4` 被写入该 A。未匹配域名只走 china，`gfw4` 不变。
- **bypass_cn**（`t14-bypass-cn-t1`）：
  - `chn` 列表 → 只走 china；`gfw` 列表 → 只走 trust；
  - 自定义组 `uproxy` → trust，写 `uproxy4`；`udirect` → china，写 `udirect4`；
    `boot` → direct（china），无 ipset；
  - `tag:none` 且 china 返回中国 IP → 接受 china；china 返回非中国 IP → 等 trust，最终用 trust；
  - `no-ipv6` 的 AAAA、`filter-qtype 65` 的 HTTPS 都在本地回 NODATA，**不产生上游查询**。
- **global**（`t14-global-t1`）：`apple` 列表 → china；其他 → trust（默认 gfw）。
- **T2 / 显式协议**（`t922-mixed`、`t922-explicit-tcp`）：客户端 UDP 时 trust 走 `udp://` 端点；
  客户端 TCP 时，未指定协议的 china/trust 都改走 TCP（fake 上游分别记录到 `:tcp`）。
- **T3/WAN**（`t923-wan-t3`）：trust = `202.96.x`/114 等直连 IP，回答来自 202.96.128.86。
- **不过滤配置**（`t14-no-v6-filter`）：AAAA、HTTPS 都正常转发并返回。
- **cache**（`t14-single-trust`）：同一 A 查询第 2、3 次**不再命中上游**，返回缓存结果。
- **EDNS**：带 OPT 的查询上游可见 OPT，客户端响应 `ar=0`（chinadns-ng 裁掉 additional）。

这些正是 M1/M2 需要逐条对齐的 P0 行为，已经机器可读地固定下来。

### 4.5 输出物与用途

| 文件 | 用途 |
|---|---|
| `coverage.json` | 配置指令覆盖；新增 fixture 时对比 |
| `startup.json` | 22 份配置启动基线 |
| `baseline.json` | M1/M2 行为 diff 的 golden（上游命中、响应、set 内容） |
| `wire-samples.json` | 真实 wire 样本；parser 回归 |
| `probe.json` + `edge-wire-samples.json` | 边界查询行为 |

M1 的验收方式：用同一 harness 把 SUT 从 chinadns-ng 换成 `aether-dns`，对 `baseline.json`
逐字段 diff；先要求 29 条查询的响应/命中/set 全部一致，再扩 fixture。

### 4.6 限制（已登记，不是缺陷）

- 假上游只覆盖 A/AAAA/HTTPS/NXDOMAIN；没有 CNAME/DoT/大响应/TTL 边界（P0 不需要 DoT；
  CNAME 由 parser 合成语料覆盖）。
- 时序用固定延迟让“首个合法响应”可复现；不是长稳/并发测试。
- 需要 root + netns + mount ns；只用于 lab/CI，不是运行时依赖。
- Python harness 是否在 M1 保留或改写成 Rust 测试，见 IMP-006。

### 4.7 结论

**GO。** fixture 不再是“静态文本”，而是可启动、可查询、可记录集合写入的基线入口；
M0 已经用它验证了 22/22 配置和 13 条 P0 行为事实。

---

## 5. 失败/被否方案汇总

| 类别 | 方案 | 否决理由 | 替代 |
|---|---|---|---|
| nftset | `nft` CLI 实现 | ~23.7 ms/次，3 个数量级慢，错误语义不可控 | 纯 netlink |
| nftset | libnftnl/libmnl | C 依赖、OpenWrt 交叉编译负担 | 纯 Rust + libc |
| nftset | 启动 dump 全量缓存 | 区间配对复杂、reload/并发失效 | 逐 IP `GETSETELEM` |
| nftset | 无 ACK 写入 | 不可确认、500 ms 超时 | batch + ACK |
| nftset | `NLM_F_EXCL` | 已存在即整批失败 | test-then-add |
| parser | hickory `to_vec()` 转发 | 字节改变、丢尾部、多 question 畸形、保留 OPT | raw forward + 偏移扫描 |
| parser | 手写完整 parser | 旧项目失败路径、无必要 | hickory + 薄适配 |
| parser | 用 hickory 做门禁 | 与 chinadns-ng 宽松检查不一致 | 宽松 header 检查 + raw |
| harness | 真实公共 DNS | 不确定、等于启动真实服务 | netns 假上游 |
| harness | 改写配置路径 | 破坏 golden 字节 | mount ns + tmpfs |
| harness | 直接写宿主 `/etc` | 污染宿主、不可并行 | mount ns |

---

## 6. 设计缺口（已登记 IMPROVEMENTS，未批准不实现）

已写入 [`IMPROVEMENTS.md`](IMPROVEMENTS.md)：

- **IMP-004 nftset 更新策略与 nft CLI 互操作**：test→batch add→ACK→EEXIST 重试；不缓存
  handle；EPERM/ENOENT 记录并按“不在集合”处理；串行化写。parity 影响 `compatible`。
- **IMP-005 转发路径：raw-forward + 字节偏移薄适配**：hickory 只解析；原地 ID/RA/TC/TTL/
  additional 操作；不重编码。parity 影响 `compatible`，边界畸形包的差异列为 known divergence。
- **IMP-006 harness/baseline 作为 M1/M2 验收入口**：Python 保留还是 M1 改 Rust 测试工具，
  需要所有者决定；baseline 格式作为 diff 契约。

这三项都不是行为改进，而是实现决策/测试基础设施；按项目规则仍需批准后进入 M1。

---

## 7. Go/No-Go 到 M1

### 7.1 判定

**GO。** 停止条件“nftset 没有可靠 netlink 方案”未触发；三个 spike 均有可复跑证据。

### 7.2 M1 入口条件（建议写入 PLAN）

1. 所有者批准 IMP-004/005/006 的实现决策（或指定替代）。
2. M1 骨架按本报告结论实现：
   - `nftset`：移植 `spikes/m0-nftset` 的 netlink 模块（纯 Rust/libc），按 IMP-004 策略；
   - `parser`：hickory 0.25.x + raw forward + 偏移扫描（ID/RA/TC/additional/TTL）；
   - config/`-C`、UDP/TCP 监听 127.0.0.1:31053、单组单上游转发、`--version`/`--help`；
   - 不改 aether-proxy；`chinadns_bin` 仍可回退到 chinadns-ng。
3. M1 验收：
   - 22/22 `expected/chinadns.conf` 可解析并启动；
   - harness 的 9 场景/29 查询与 `baseline.json` 对齐（先做单组/转发，再做组/列表/tag）；
   - 用真实 chinadns-ng 作为 oracle，不允许“猜行为”。
4. 不进入 P1：DoT、hosts、cache-db、完整 6 组、IPv6 代理等仍不在 M1。

### 7.3 风险与缓解（M1）

| 风险 | 证据/缓解 |
|---|---|
| netlink 集合在 reload 后消失 | 已验证按名字解析、无需 handle；reload 后下一次写自动恢复 |
| 并发写导致 EEXIST | test-then-add + 重试；M1 串行化写入 |
| hickory 与 chinadns-ng 边界差异 | raw forward；差异登记，P0 不受影响 |
| cache TTL 原地改写 | 偏移扫描器已在 M0 验证；M2 再做 cache |
| 时序/并发导致的基线抖动 | 假上游固定延迟；baseline 允许“同一组内端点二选一”的字段级比较 |

### 7.4 明确不做

- 不因为 M0 通过就自动进 M1；等所有者确认。
- 不把 spike 代码直接当生产代码提交；M1 重写/移植并加测试。
- 不新增 P1/P2 功能；不修改 aether-proxy。
- 不把“nft CLI add 失败”当成 aether-dns 的 bug 去修（上游既有行为，aether-proxy 不依赖）。

---

## 8. 证据索引与复现

### 8.1 文件

| 文件 | 内容 |
|---|---|
| [`spikes/README.md`](spikes/README.md) | spike 目录与复现说明 |
| [`spikes/m0-nftset/evidence.log`](spikes/m0-nftset/evidence.log) | netlink 全量证据 |
| [`spikes/m0-nftset/src/netlink.rs`](spikes/m0-nftset/src/netlink.rs) | netlink 消息/属性/解析 |
| [`spikes/m0-nftset/src/main.rs`](spikes/m0-nftset/src/main.rs) | dump/test/add/add-safe/bench |
| [`spikes/m0-parser/evidence-corpus.log`](spikes/m0-parser/evidence-corpus.log) | 合成语料解析/重编码 |
| [`spikes/m0-parser/evidence-wire.log`](spikes/m0-parser/evidence-wire.log) | 38 个真实 wire 样本 |
| [`spikes/m0-parser/evidence-edge.log`](spikes/m0-parser/evidence-edge.log) | 边界 wire 样本 |
| [`spikes/m0-parser/src/main.rs`](spikes/m0-parser/src/main.rs) | hickory 语料 + 偏移扫描器 |
| [`spikes/m0-fixtures/coverage.json`](spikes/m0-fixtures/coverage.json) | 指令覆盖 |
| [`spikes/m0-fixtures/startup.json`](spikes/m0-fixtures/startup.json) | 22/22 启动 |
| [`spikes/m0-fixtures/baseline.json`](spikes/m0-fixtures/baseline.json) | 行为基线（9 场景/29 查询） |
| [`spikes/m0-fixtures/wire-samples.json`](spikes/m0-fixtures/wire-samples.json) | 真实 wire 样本 |
| [`spikes/m0-fixtures/probe.json`](spikes/m0-fixtures/probe.json) | 边界查询行为 |

### 8.2 复现命令

```bash
# 工具链（一次性）
sudo apt-get install -y rustc cargo nftables
# chinadns-ng 2025.08.09 x86_64（SHA256 见上）
mkdir -p /tmp/ci-tools/chinadns-ng-2025.08.09
curl -L -o /tmp/ci-tools/chinadns-ng-2025.08.09/chinadns-ng \
  'https://github.com/zfl9/chinadns-ng/releases/download/2025.08.09/chinadns-ng+wolfssl@x86_64-linux-musl@x86_64@fast+lto'
chmod +x /tmp/ci-tools/chinadns-ng-2025.08.09/chinadns-ng

cd spikes/m0-nftset && ./run-spike.sh
cd spikes/m0-parser && ./run-spike.sh
cd spikes/m0-fixtures && ./run-harness.sh all
```

`run-harness.sh` 需要 root；它会创建 `aether-m0` netns，退出后保留（可
`sudo ip netns del aether-m0`），不修改宿主文件，不访问真实 DNS。

### 8.3 未覆盖/留给 M1

- `tls://` 上游：22 份 fixture 都没有使用，P0-conditional 仍未验证；M1 不做。
- CNAME/DoT/大响应/EDNS 选项矩阵：parser 合成语料覆盖 CNAME，harness 未覆盖；
  在 M2 行为测试中补。
- 真机/OpenWrt：M0 只在 WSL2 netns 验证；M3 再做。
- 长稳/并发：M0 明确不做；不进入 P0 门禁。

---

## 附录 A：M0 关键数字

- nftset：test 5.5 µs/次；batch add 50–90 µs；500 IP 2.77 ms；`nft` CLI ~23.7 ms/次。
- parser：合成 17/20 parse（3 个畸形双方拒绝）；真实 wire 38/38；重编码 10/17 字节不同。
- harness：22/22 启动；19/19 指令；9 场景/29 查询；13/13 sanity。
- 基线二进制：chinadns-ng 2025.08.09，SHA256 `842ea4e9…`。

## 附录 B：与 chinadns-ng 的已知边界差异（M0 观察，未修）

| 输入 | chinadns-ng | 建议 aether-dns | 状态 |
|---|---|---|---|
| 2 question / qdcount=0 / label-64 / 11 字节 | 回“坏响应”，不转发 | M1 先宽松解析 + raw forward；是否复刻留 P1 | 观察 |
| 尾部垃圾 | 原样转发 | 原样转发 | 一致 |
| 查询 TC 位 | 清零后转发 | 清零后转发 | 一致 |
| 响应 additional/OPT | good 响应裁掉 | 裁掉 | 一致 |
| 响应 `RA` | 置 1 | 置 1 | 一致 |
| 响应 ID | 重写为客户端 ID | 重写 | 一致 |
| TTL | cache 命中时原地递减 | 原地递减 | M2 验证 |

（“一致”指 M0 已观察到 chinadns-ng 的行为；aether-dns 尚未实现，M1/M2 按此验收。）
