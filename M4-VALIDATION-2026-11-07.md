# M4-01 缓存刷新真机验收（2026-11-07）

设备 H28K，aether-dns 包 `2026.11.04-1`，源码 `325b614`，运行二进制 SHA256 `ba17e8b9401c0183360148555a59e2b6b93ba651e4df01bd360452e9f4caa3b9`。安装记录见 [M4-DEPLOYMENT-2026-11-07.md](M4-DEPLOYMENT-2026-11-07.md)。

## 低频 TTL 复验

13:35:24–13:45:38（UTC+08:00），直接在H28K查询127.0.0.1:31053的example.com A，绕过dnsmasq缓存，查询完成后等待10秒，共61样本。

- 61/61 NOERROR，无SERVFAIL/超时；TTL0为1/61=**1.64%**，满足卡片<5%要求。
- TTL多次回升（不假设上游总给300秒），最大dig耗时4ms。
- boot ID与aether-dns PID前后一致；最终running/T1。

## 上游请求量复验

通过SSH TCP转发直接访问31053，计数表只计数、不拦截。根据upstream.rs每次TCP exchange新建连接、每次UDP exchange发送一条请求的实现，计数以下流量：本地Xray DNS tunnel 31082/31083的TCP SYN、非loopback TCP/53 SYN、非loopback UDP/53请求。

- 阳性对照：3次随机google.com子域查询，trust TCP计数增加6，证明两个上游并发可被计数。
- 30秒生产背景流量：TCP+8、UDP+4，约0.40次/秒；不以此作背景扣减。
- 预热example.com的首查返回TTL0，触发刷新；该样本单独记录，未藏入下述负载统计。
- 10个并发缓存命中：10/10 NOERROR，上游计数不增加。
- 持续5 qps、360秒：**1800/1800 NOERROR，TTL0=0/1800**。耗时包含SSH转发，中位3.71ms、p95 4.21ms、最大17.33ms。
- 负载期间总上游计数：trust TCP+76、direct TCP+0、direct UDP+44；**120次/360秒=0.333次/秒**，约为客户端请求量的6.67%。
- 计数包含其他生产请求、多上游并发及SYN重传，是测试域名上游尝试量的保守上界，不能解释为example.com精确刷新次数。

## 判定与限制

**M4-01卡片要求的这组真机指标通过**：TTL0<5%、无SERVFAIL、持续命中/并发突发下上游量有界，未观察到逐客户端查询放大或刷新风暴。

这不是所有域名和长期负载的保证。真机未额外注入失败来测冷却或内部并发4上限，相关证据来自本地测试。未验证IPv6/DoT等额外路径。两轮测试均未重启服务或设备、未修改配置；临时计数表与SSH转发均已清理，最终running/T1。

原始证据位于忽略目录 `dist/logs/t15.1-install/`，含cache-retest.log、cache-retest-summary.json、cache-qps-result.json、cache-qps-queries.json和脚本。本文件是正式验收摘要，不提交原始设备日志。

跨项目M4-04已映射aether-proxy T15.1，真机验收通过并合并为`0e0aac4`；其详细记录保存在aether-proxy仓库 `docs/validation/t15.1-h28k-20261107.md`。
