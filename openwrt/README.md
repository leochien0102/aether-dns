# OpenWrt 包定义

本目录仅打包预先构建好的二进制，不在 SDK 内编译 Rust，也不下载二进制。

## 输入与产物

将目标架构的静态 musl 二进制放到 `openwrt/files/aether-dns`（该文件不入库）。
生产输入由 M4-10 提供；配方不做二次 strip，仅安装 `/usr/bin/aether-dns`。
`PKG_VERSION:=1.0.0`、`PKG_RELEASE:=1` 在本地 24.10.8 SDK 中生成包版本
`1.0.0-r1`，二进制 `-V` 版本保持 `1.0.0`。

`PROVIDES:=dns-engine` 用于虚拟依赖；清空 `PKG_DEFAULT_DEPENDS`，避免为静态输入注入
`libc` 依赖。`DEFAULT_VARIANT:=1` 保留供 apk 使用，ipk control 不含此字段。
不安装配置或 init 单元，不定义包特有安装脚本；SDK 自动生成的默认转发脚本保留。

## 本地 SDK 验证

从本卡 worktree 根目录运行（SDK 已在本地，不需下载/安装工具）：

```sh
SDK=/workspace/openwrt/sdk/openwrt-sdk-24.10.8-rockchip-armv8_gcc-13.3.0_musl.Linux-x86_64
mkdir -p openwrt/files openwrt/build
# 先放入 openwrt/files/aether-dns；不要覆盖 SDK 中其他包目录。
# 首次接入时执行；如果已有本卡链接则无需重复：
test ! -e "$SDK/package/aether-dns" && ln -s "$PWD/openwrt" "$SDK/package/aether-dns"
# 更新本地输入后 clean，确保 Prepare 重新拷贝。
make -C "$SDK" package/aether-dns/clean V=s
make -C "$SDK" package/aether-dns/compile V=s
IPK="$SDK/bin/packages/aarch64_generic/base/aether-dns_1.0.0-r1_aarch64_generic.ipk"
tar -xzOf "$IPK" ./control.tar.gz | tar -xzO ./control
tar -tzf "$IPK"
tar -xzOf "$IPK" ./data.tar.gz | tar -tzvf -
tar -xzOf "$IPK" ./control.tar.gz | tar -xzO ./postinst
tar -xzOf "$IPK" ./control.tar.gz | tar -xzO ./prerm
```

control 应包含 `Package: aether-dns`、`Version: 1.0.0-r1`、
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
