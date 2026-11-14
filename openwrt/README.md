# OpenWrt 包定义

本目录只服务本仓库 CI 的本地二进制打包，不在 SDK 内编译 Rust，也不下载二进制。
feeds 打包已移到 aether-proxy 仓库的 `aether-dns/Makefile`，由其下载 Release 裸二进制。

## 输入与产物

将目标架构的静态 musl 二进制放到 `openwrt/files/aether-dns`（该文件不入库）。
生产输入由 M4-10 提供；配方不做二次 strip，仅安装 `/usr/bin/aether-dns`。
`PKG_VERSION:=1.0.2`、`PKG_RELEASE:=1` 在本地 24.10.8 SDK 中生成包版本
`1.0.2-r1`，二进制 `-V` 只输出核心 `aether-dns 1.0.2`。

Release 资产为四个 ipk/apk 包、两个无扩展名裸二进制
`aether-dns-1.0.2-r1-aarch64_generic` / `aether-dns-1.0.2-r1-x86_64` 及 `SHA256SUMS`。
架构记号为包架构，不是 OpenWrt 的 `ARCH`。**GitHub Release 不保留可执行位**，
消费方下载后必须 `chmod +x`；CI 内部裸二进制按 0755 校验。
`v1.0.2` tag/Release 由设计负责人发布并回填，不能把本地验证当成已发布。

`PROVIDES:=dns-engine` 用于虚拟依赖；清空 `PKG_DEFAULT_DEPENDS`，避免为静态输入注入
`libc` 依赖。`DEFAULT_VARIANT:=1` 保留供 apk 使用，ipk control 不含此字段。
不安装配置或 init 单元，不定义包特有安装脚本；SDK 自动生成的默认转发脚本保留。

## 本地 SDK 验证

从本卡 worktree 根目录运行（使用本地 SDK 的隔离副本，不写共享 SDK、不覆盖其他卡的链接）：

```sh
SDK="$PWD/openwrt/build/sdk"
mkdir -p openwrt/files openwrt/build
# SDK 副本须预先准备在上述目录；将副本中的旧绝对链接重定位到副本，
# 移除指向其他仓库的 feed 链接/配置；共享 SDK 保持只读。
# package/aether-dns 链接须指向本 worktree 的 openwrt/。
# 先放入 openwrt/files/aether-dns；不要覆盖 SDK 中其他包目录。
test "$(readlink "$SDK/package/aether-dns")" = "$PWD/openwrt"
# 更新本地输入后 clean，确保 Prepare 重新拷贝。
make -C "$SDK" package/aether-dns/clean V=s
make -C "$SDK" package/aether-dns/compile V=s
IPK="$SDK/bin/packages/aarch64_generic/base/aether-dns_1.0.2-r1_aarch64_generic.ipk"
tar -xzOf "$IPK" ./control.tar.gz | tar -xzO ./control
tar -tzf "$IPK"
tar -xzOf "$IPK" ./data.tar.gz | tar -tzvf -
tar -xzOf "$IPK" ./control.tar.gz | tar -xzO ./postinst
tar -xzOf "$IPK" ./control.tar.gz | tar -xzO ./prerm
```

control 应包含 `Package: aether-dns`、`Version: 1.0.2-r1`、
`Architecture: aarch64_generic`、`Provides: dns-engine`、`License: GPL-3.0-or-later`，
无 `Depends: libc`。数据段只有目录项和 `./usr/bin/aether-dns`（0755）。

### 离线 root 安装/卸载

仅使用 worktree 内的离线 root，不安装设备：

```sh
ROOT="$PWD/openwrt/build/offline-root"
OPKG="$SDK/staging_dir/host/bin/opkg"
CONF="$PWD/openwrt/build/opkg.conf"
TMP="$PWD/openwrt/build/tmp"
mkdir -p "$ROOT/tmp" "$TMP"
printf 'arch all 1\narch aarch64_generic 10\n' > "$CONF"
"$OPKG" -f "$CONF" -o "$ROOT" --offline-root "$ROOT" --tmp-dir "$TMP" install "$IPK"
# 仅当输入能在宿主执行时（例如本卡最小 shell stub）：
"$ROOT/usr/bin/aether-dns" -V
"$OPKG" -f "$CONF" -o "$ROOT" --offline-root "$ROOT" --tmp-dir "$TMP" status aether-dns
"$OPKG" -f "$CONF" -o "$ROOT" --offline-root "$ROOT" --tmp-dir "$TMP" remove aether-dns
test ! -e "$ROOT/usr/bin/aether-dns"
```

本卡验证使用最小 shell stub，仅证明包结构、元数据与安装行为，不是 DNS 引擎，
不可部署。真实 aarch64 二进制不可直接在 x86_64 宿主运行。
静态性：**委托 M4-10 验证**；apk 的 Default-Variant 验证也归 M4-10。
