include $(TOPDIR)/rules.mk

PKG_NAME:=aether-dns
PKG_VERSION:=1.0.0
PKG_RELEASE:=1

PKG_LICENSE:=GPL-3.0-or-later
PKG_MAINTAINER:=leochien0102 <leochien0102@users.noreply.github.com>

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
  Packages a prebuilt static musl aether-dns binary supplied locally.
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
