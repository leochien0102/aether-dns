# M4-01 / M4-07 审查、构建及 H28K 安装记录

日期：2026-11-07（UTC+08:00）。本次范围为所有者授权的夜间审查、合并、Rockchip 包构建和安装；**不重启 H28K，不刷固件**。允许重启代理/DNS 服务。完整真机复验留给所有者白天执行。

## 1. 源码与审查

| 项目 | 提交 / 结果 |
|---|---|
| M4-07 实现 | `0b5b52ab32f2d0f7b7e7f1b72fd6685d5648d752` |
| M4-07 合并 | `b2acecf` |
| M4-01 实现 | `ec69b4e`（重新实现，不使用已撤销的旧实现） |
| M4-01 纳入测试修复后的复验树 | `f73014cdd348f2c91ad4410efb9687d304bc9047` |
| 最终构建源码 | `325b614a7bb927cc94dc6d7b38751010e9be8531` |

M4-07 提交、干净工作区、已提交交回报告持续稳定五分钟后进入复审。

- 核对代码范围：M4-07 仅测试 harness 与报告；M4-01 仅 cache/server、对应测试与报告。
- M4-07 报告的十轮全量日志各为 212 passed；三轮 review 均 `FAIL=0`。
- 独立执行 M4-07 review、startup_retry、三轮 TCP/UDP 双进程测试，全部通过。
- M4-07 合入 main 后 review 通过；M4-01 合入其修复后 review 与五轮 cache/cache_refresh 通过。
- M4-01 合入 main 后最终 review 为 `FAIL=0`，20 个测试目标合计 224 项（以原始日志核对）。
- 实现报告按仓库约定未保留在 main；可由各实现提交读取。未 push，未派实现子智能体。

## 2. Rockchip 构建

构建根：`/workspace/openwrt/openwrt-actions/build/rockchip`。

本地构建入口为 `package/aether-dns-local/Makefile`；源码由已通过审查的 main 使用 `git archive` 导出到其 `source/`，不是从未提交工作区复制。只执行包目标：

```sh
make -j2 package/aether-dns-local/compile CONFIG_PACKAGE_aether-dns=m V=s
```

- 使用已有 Rust 1.99.1 主机工具链；官方 `rust-std-1.99.1-aarch64-unknown-linux-musl.tar.xz` 经其 SHA256 文件校验。
- 标准库放在构建树的独立 staging sysroot；没有修改系统 Rust 或用户 rustup 安装。
- 链接器为 Rockchip 构建树的 `aarch64-openwrt-linux-musl-gcc`。
- 产物是静态 ARM64 ELF，无动态段。
- 未运行固件构建、源码更新或 feeds 更新脚本。
- 未做 M4-03 semver 迁移：`-V` 保持 `2026.11.04`，opkg 包修订号为 `2026.11.04-1`。包内新增只读构建标识 `/usr/share/aether-dns/build-info.txt`，含源码提交。

产物：`openwrt-actions/build/rockchip/bin/packages/aarch64_generic/base/aether-dns_2026.11.04-1_aarch64_generic.ipk`。

| 校验对象 | SHA256 |
|---|---|
| ipk | `0ef0212c53071f5eb402cd02d51d8e91f981ccce0143efd4231c970208a7dabf` |
| 包内及运行中二进制 | `ba17e8b9401c0183360148555a59e2b6b93ba651e4df01bd360452e9f4caa3b9` |
| 原二进制 | `7f8330f469e7afcbe1753a08ac54f6357caba38200c78ea790c31ff2da42dec1` |

## 3. 安装与基础核对

- 安装前备份：H28K `/root/aether-dns-before-m4-20261107/`，含旧二进制、旧 ipk、代理配置、包状态与设备 boot ID。
- 通过 scp 上传；校验新 ipk、新二进制和备份旧 ipk；临时新二进制 `-V` 可运行。
- 上传阶段一次本地工具期限过短终止；先检查远端现状，再补传并校验。安装仅执行一次。
- `opkg install` 从 `2026.11.04` 升至 `2026.11.04-1` 成功。
- 执行一次 `/etc/init.d/aether-proxy restart`；没有执行 reboot、poweroff、sysupgrade 或看门狗复位。
- 00:09:54 基础核对：`running / T1 / bypass_cn`，健康探针成功，aether-dns PID 29579。
- `/proc/29579/exe` 与安装路径哈希均匹配新包。
- 本机 31053 及 dnsmasq 53 各一次查询均 NOERROR。
- 安装前后 boot ID 均为 `97edbacd-fd63-4a21-beb7-0cc59ac22b93`，uptime 连续增长，证明设备未重启。

这只证明安装、运行与基本解析正常，**不宣称缓存刷新真机指标已经验收**。

## 4. 白天复验入口

1. 核对包版本、构建提交、运行中 ELF 哈希和 `running/T1`。
2. 缓存刷新试验应直接查 aether-dns 的本机 31053，避免 dnsmasq 二级缓存遮蔽刷新命中；每 10 秒查询同一个 A 记录至少 10 分钟，记录 TTL/rcode/耗时。
3. 验收卡要求 TTL0 占比 <5%、无 SERVFAIL、上游查询频率有界；上游 QPS 需要单独可核对的观测，不可仅从 TTL 推断。
4. 单独验证 LAN 53 的用户可见表现；两条查询路径分开统计。
5. 失败冷却、同 key 去重、并发上限与 shutdown 已有受控本地测试；若要真机故障演练，另行确认窗口。
6. 不自动重启设备，不借本次安装实施 M4-02–06 的命名、版本或代理启停编排修改。

## 5. 回退（仅需要时执行）

旧包已存设备持久目录；回退只需包降级和代理服务重启，不需要重启设备：

```sh
opkg install --force-downgrade /root/aether-dns-before-m4-20261107/old.ipk
/etc/init.d/aether-proxy restart
```

回退后核对原二进制哈希和 `running/T1`，不擅自改配置。

## 6. 本地证据

`dist/logs/m4-nightly/`（不入库）保存独立审查、组合测试、最终 main 门禁、构建、上传校验、备份、安装及安装后状态日志。Rockchip 本地包定义与源码快照留在构建树，后续不得误当作已发布 feed 包。
