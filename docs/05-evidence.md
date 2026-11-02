# 05 依据、假设与许可边界

调研日期：2026-11-02。只读核对；未编译新程序、未重跑已有 DNS 验证、未操作实际路由器。

## 1. 本地项目证据

| 来源 | 本稿使用的事实 |
|---|---|
| aether-proxy `docs/03-dns.md` | 三种模式、T1/T2/T3、boot/uproxy/udirect、过滤、dnsmasq 接口、LKG |
| aether-proxy `aether-proxy/src/lib/render/chinadns.uc` | 实际上游展开、模式降级、gfw 设备覆盖、自定义组与监听配置 |
| aether-proxy `docs/02-dataplane.md` | nft 集合及 interval 要求、整表替换与现有主机策略 |
| aether-proxy `aether-proxy/src/lib/render/xray.uc` | DNS tunnel 显式转到节点，不需要为此引入 SOCKS 转换器 |
| aether-proxy-channel `.github/workflows/packages.yml` | 现有 SDK 构建 x86_64/aarch64 与分离发布的背景 |
| helloworld `dns2tcp/Makefile`、`dns2socks*/Makefile` | 轻量转换器构建/功能范围；它们不是完整 DNS 策略引擎 |

核对时 aether-proxy HEAD：`7a2394212087438ed10cbba744cc6aef4f31443c`。工作区存在未提交 DNS/分流方案，因此该 commit 不代表本文全部设计输入；特别是 dnsmasq 替换段与 `13-routing.md` 仍为草案，不作为已实现能力。

本文不修改这些项目，不把其他工作区草案作为已批准决定。后续 P0 需归档准确的兼容夹具和基线哈希，不仅引用易变工作区。

## 2. 上游一手来源

- [ChinaDNS-NG README](https://github.com/zfl9/chinadns-ng/blob/master/README.md)：Zig 0.10.1 + C、组内并发、域名最长后缀、同后缀归属、TCP/DoT、nft、缓存。
- [ChinaDNS-NG src/dnl.c](https://github.com/zfl9/chinadns-ng/blob/master/src/dnl.c)：同键优先加载后不覆盖、后缀从具体到宽泛查询、旧域名级数限制。
- [ChinaDNS-NG LICENSE](https://github.com/zfl9/chinadns-ng/blob/master/LICENSE)：AGPL version 3 文本。正式复用前还需检查具体 commit、各文件授权和依赖，不只看根 LICENSE。
- [dns2tcp](https://github.com/zfl9/dns2tcp)：UDP→TCP 转换的窄职责；不是完整策略引擎。
- 候选协议库：[Hickory DNS](https://github.com/hickory-dns/hickory-dns)。候选异步运行时：[Tokio](https://github.com/tokio-rs/tokio)。本轮未完成依赖审计或 API 适配验证。

上游链接目前指向 master，仅证明调研所见。无法用 GitHub API 查询最新 commit（本次请求返回 403）；不在文档中编造固定 hash 或精确最后更新时间。P0 必须固定准确版本再建立对照测试。

## 3. 协议审查参考

- RFC 1035：DNS 消息、TCP framing 与基础记录。
- RFC 2308：负应答缓存（第一版暂不启用）。
- RFC 5452：伪造应答抵抗与事务匹配。
- RFC 6891：EDNS(0)。
- RFC 7766：DNS over TCP。
- RFC 7858 / RFC 8310：DoT 与认证。
- RFC 8767：过期应答的设计参考；本稿 TTL=0 等条款是候选产品策略，不声称已经完整实现 RFC 的所有推荐。
- RFC 9267：DNS 名字解压与实现安全陷阱。

引用标准不等于合规已验证；P1 应按支持范围生成协议测试清单。

## 4. 未验证的关键假设

1. 候选协议库在所需 feature 下能安全处理未知 RR/压缩/EDNS，同时满足资源预算。
2. Tokio 单 worker 与选定 netlink 实现能够在慢上游和重载下保持国内查询响应。
3. 真实 OpenWrt 内核 interval 集合查询/写入能通过小而可维护的 Rust 模块实现。
4. 目标 SDK、musl、Rust 依赖及 CPU 特性组合可稳定交叉构建并运行。
5. 规则与缓存预算适合首轮目标设备；本稿没有任何新引擎实测数字。
6. 外层生命周期能支持集合屏障与配置 ACK；不支持时需修改集成或降低保证。

## 5. 许可与独立实现

- Rust 重写不是自动免责方式。复制、翻译、改编上游实现或测试素材，需要逐项判断许可与衍生关系。
- 当前仅编写方案，没有引入第三方源码、上游实现测试、静态库或二进制。
- 若走独立实现，应按公开行为/协议编写自己的规格和测试；保留来源与设计依据。已阅读上游代码的事实应如实记录，不能声称采用未经实施的 clean-room 流程。
- 若决定改编上游，明确采用满足其许可的路线，不以变量改名或语言变化规避 AGPL；独立进程边界也不是规避许可的万能保证。
- 所有者审定项目许可证后，才添加 LICENSE、Cargo 元数据与对外复用承诺；依赖、数据、代码和文档许可分别核对。
- 公开发布前检查对应源码、构建脚本、依赖许可通知和第三方归属。需要法律确定性的部分寻求专业审查，本文不是法律意见。
