# aether-dns M3 报告：H28K 真机集成验收

> 日期：2026-11-06。范围：**M3 真机集成**（aether-proxy 真实使用 aether-dns），不进入 M4。
> 结论：**通过，保持 aether-dns 2026.11.04 运行**；观察期按所有者要求由 24–72 h 缩短为
> **11.8 h（10:23–22:13）**，146 个样本无阻断性故障。发现三个重启期缺陷，均在
> aether-proxy 侧，已转 M4-04/05/06，M3 期间不改代码。

## 0. 结论摘要

| 验收 | 结果 |
|---|---|
| M3-01 安装/切换/回滚/冷重启 | 通过（安装 09:51:14，首次使用 09:59:09，回滚演练 13 s，冷重启后自动恢复） |
| M3-02 模式矩阵 | 通过：`gfw`、`global`、`bypass_cn` 均生效，结束恢复 `bypass_cn` |
| M3-03 T1/T2/T3 | 通过：T2 约 48 s、T3 约 50 s、回 T1 11–15 s |
| M3-04 生命周期/故障/回滚 | 通过：kill child、stop/start、Xray down、blackhole 降级、回滚均有记录 |
| M3-05 观察 | 缩短为 11.8 h；146/146 `running`、`T1`、NOERROR；无非预期中断 |
| M3-06 报告 | 本文件 |

当前状态：`state: running`、`dns_tier: T1`、`mode: bypass_cn`，
`components.chinadns-ng.path=/usr/bin/aether-dns`、`version=2026.11.04`；
原 `/usr/bin/chinadns-ng` 未覆盖；回滚资产在设备 `/root/aether-dns-rollback-20261106/`。

## 1. 环境

- H28K，192.168.5.254，OpenWrt 24.10.5 / LEDE，kernel 6.18.60，aarch64_generic。
- aether-proxy 0.6.3-r4，17:21 升级到 0.6.3-r5；Xray 26.10.31。
- aether-dns 2026.11.04（静态 aarch64 musl），ipk SHA256
  `077929616c8b870c785f493e327e32824b3d65a0e6055fd59506fa085ebed1c1`。
- 切换方式：UCI `aetherproxy.main.chinadns_bin=/usr/bin/aether-dns`，不覆盖原二进制。
- 日志：`dist/logs/h28k-20261106/`（功能验证）、`dist/logs/h28k-weekend/`（观察）。

## 2. M3-01 安装、切换、回滚、冷重启

| 时间 | 事件 |
|---|---|
| 09:50 | 备份原 chinadns-ng、上传 ipk |
| 09:51:14 | `opkg install` 完成（`Installed-Time 1793929874`），仅新增 `/usr/bin/aether-dns` |
| 09:52–09:57 | 旁路自测（127.0.0.1:31054），未接入 aether-proxy |
| 09:59:09 | 首次真实使用：`SERVICE_STARTED`，`chinadns_bin` 指向 aether-dns |
| 09:59:30–09:59:43 | 回滚演练到原 chinadns-ng（13 s），DNS 正常，原二进制 SHA256 未变 |
| 10:01:57 | 切回 aether-dns |
| 10:04:38 | 冷重启（已授权） |
| 10:05:25 | 重启后 aether-proxy 自动拉起，aether-dns 继续服务 |

证据：`install.log`、`switch-and-verify.log`、`rollback-and-verify.log`、
`reboot-persistence.log`、`full-validation.md`。

## 3. M3-02 模式矩阵

| 模式 | status | CN `www.baidu.com` | GFW `www.google.com` | LAN |
|---|---|---|---|---|
| `gfw` | running/T1 | NOERROR | NOERROR | NOERROR |
| `global` | running/T1 | NOERROR（经代理） | NOERROR | NOERROR |
| `bypass_cn` | running/T1 | NOERROR | NOERROR | NOERROR |

结束恢复 `bypass_cn`；未改 nft 生产规则之外的内容。
证据：`mode-matrix.log`、`m3-mode-tier.md`。

## 4. M3-03 T1/T2/T3

方法：对节点 IP `20.243.137.248/32` 加临时 `ip route add blackhole`，让健康检测失败；
分别在有/无 `direct_dns_enabled` 时观察 T2/T3；撤销后 `probe now` 回 T1。

- **T2**：约 48 s 进入；`example.com` 正常，`www.google.com` SERVFAIL（直连 DNS 不可达被墙域名）。
- **T3**：约 50 s 进入；`example.com` 正常，`www.google.com` NOERROR（国内 DNS 污染/非最优答案）。
- **恢复**：11–15 s 回 `running/T1`；`direct_dns_enabled` 已恢复未设置，blackhole 已删除。

结论：降级/恢复行为符合 aether-proxy 设计周期，不是 aether-dns 缺陷。
证据：`tier-drill.log`、`t2-t3-blackhole.log`、`t3-blackhole.log`、`m3-final-state.log`。

## 5. M3-04 生命周期与故障

- `kill -TERM` aether-dns：`CHILD_CRASHED ... restart_in=5`，5–7 s 后新 PID 恢复。
- `aether-proxy stop` → 约 1 s 停止；`start` → 约 12 s 恢复，aether-dns 重新监听。
- Xray 终止：触发 T2/T3 与 `MODE_DEGRADED`，恢复后回 T1。
- 回滚到原 chinadns-ng 后 DNS 正常，再切回 aether-dns。
- 全程未 reboot；唯一一次冷重启在 M3-01，有明确授权。

证据：`lifecycle.log`、`post-kill-state.log`、`crash-restart-clean.log`、`dns-matrix.log`、
`switch-rollback.md`。

## 6. M3-05 观察（11.8 h，所有者批准缩短）

采样：每 5 分钟只读查询，日志 `dist/logs/h28k-weekend/sample-2026-11-06.log`。

| 指标 | 值 |
|---|---|
| 样本数 | 146（10:23:36–22:13:02） |
| 采样间隔 | 中位 300 s，p95 300 s，最大 374 s；>360 s 仅 1 次（开局） |
| `state: running` | 146/146 |
| `dns_tier: T1` | 146/146 |
| `example.com` NOERROR | 146/146 |
| TTL=0 占比 | 258/292 = 88.4% |
| 非预期重启/崩溃 | 0（17:21 升级、21:10/21:12 受控测试除外） |

资源：aether-dns RSS 9.26 MB → 9.34 MB，fd 43 → 43（无泄漏迹象）；
Xray fd 161 → 185（连接数波动，非单调）。

TTL0 的成因已定量：约 300 s 新鲜 + 3600 s stale，形成 65 分钟周期；
客户端约 88% 时间拿到 TTL=0，是 M4-01 的直接输入。

## 7. 新发现：重启期三类缺陷（M4-04/05/06）

2026-11-06 21:12 授权受控重启的客户端实测（随机 gfw 域名，10 qps）：

| 区间 | 现象 | 归属 |
|---|---|---|
| 21:11:57.5–21:12:01.5 | 超时 | dnsmasq 重启 |
| 21:12:01.5–21:12:07.7 | **NXDOMAIN、约 300 ms（直连 223.5.5.5）** | M4-04 P0 |
| 21:12:07.7–21:12:11.6 | 超时 | 第二次 dnsmasq 重启 |
| 21:12:11.8–21:12:13.6 | SERVFAIL ×58（child 已监听、Xray 未监听） | M4-05 P1 |

- M4-04：停止路径删除 `/tmp/dnsmasq.d/aether-proxy.conf`（含 `no-resolv`）并重启 dnsmasq，
  回落到 `resolv-file` 223.5.5.5，约 6 s 直连泄漏；这段不产生 aether-dns 日志。
- M4-05：就绪门只等 TPROXY 31080，未等 Xray 的 DNS inbound 31082/31083；
  17:21 同类事件只留下 6 条 refused，容易低估。
- M4-06：stop/start 各重启一次 dnsmasq，合计约 8 s 超时。

证据：`startup-gap-analysis.md`、`probe-restart-2026-11-06T2112.csv`、`logread-2026-11-06T1945.txt`。

## 8. 限制与未做

- 观察期 11.8 h（所有者批准），不满足原检查表的“至少 24 h”；覆盖了白天/晚间、
  升级与两次受控重启，但未覆盖夜间到次日早高峰。
- 未做：IPv6 IP 测试、DoT、LAN 客户端透明代理抓包、多设备。
- 未改任何 M2 语义；TTL0 只记录，不在 M3 修。

## 9. M4 输入

- **M4-01** cache refresh/stale：TTL0 占比 88.4%，65 分钟周期已量化 → 第一优先。
- **M4-04** restart 直连泄漏（P0）：目标 restart 期间无直连窗口，主动 stop 语义不变。
- **M4-05** 启动期 SERVFAIL 窗口（P1）：目标 ≤0.5 s、refused 0 条。
- **M4-06** dnsmasq 两次重启（P2）：目标一次 restart 周期 ≤1 次。
- M4-02 命名、M4-03 版本：不受 M3 结果影响，按原卡推进。

## 10. 建议

1. 保持 aether-dns 在 H28K 生产运行，保留回滚资产。
2. M4 冻结顺序：M4-01 → M4-04 → M4-05 → M4-06；M4-04 是唯一“静默错误”，不应延后。
3. 修复验收复用本次探测方法（随机 gfw 域名 + 客户端 rcode/延迟），不靠日志推断。
