include $(TOPDIR)/rules.mk

PKG_NAME:=aether-dns
PKG_VERSION:=1.0.2
PKG_RELEASE:=1

PKG_LICENSE:=GPL-3.0-or-later
PKG_LICENSE_FILES:=LICENSE
PKG_MAINTAINER:=leochien0102 <leochien0102@users.noreply.github.com>

# 本目录只服务 DNS 仓库自己的 CI 打包，输入为 ./files/aether-dns。
# feeds 打包已移到 aether-proxy 仓库的 aether-dns/Makefile，下载 Release 裸二进制。

include $(INCLUDE_DIR)/package.mk

# The prebuilt static binary needs no SDK-default libc dependency.
PKG_DEFAULT_DEPENDS:=

# The input binary is already stripped by the producer.
RSTRIP:=:

define Package/aether-dns
  SECTION:=net
  CATEGORY:=Network
  TITLE:=Aether DNS forwarder
  PROVIDES:=dns-engine
  DEFAULT_VARIANT:=1
  DEPENDS:=@(aarch64||x86_64)
endef

define Package/aether-dns/description
  Packages a prebuilt static musl aether-dns binary.
endef

define Build/Prepare
	$(INSTALL_DIR) $(PKG_BUILD_DIR)
	$(CP) ./files/aether-dns $(PKG_BUILD_DIR)/aether-dns
endef

define Build/Compile
endef

define Package/aether-dns/install
	$(INSTALL_DIR) $(1)/usr/bin
	$(INSTALL_BIN) $(PKG_BUILD_DIR)/aether-dns $(1)/usr/bin/aether-dns
endef

$(eval $(call BuildPackage,aether-dns))
