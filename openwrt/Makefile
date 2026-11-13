include $(TOPDIR)/rules.mk

PKG_NAME:=aether-dns
PKG_VERSION:=1.0.1
PKG_RELEASE:=1

PKG_LICENSE:=GPL-3.0-or-later
PKG_LICENSE_FILES:=LICENSE
PKG_MAINTAINER:=leochien0102 <leochien0102@users.noreply.github.com>

# 两种构建方式：
#  - DNS 仓库自己的 CI 在构建时把二进制放进 ./files/aether-dns（不在 git 里），走本地文件；
#  - 外部构建（如固件 feed）没有该文件，从已发布的 ipk 下载并解出二进制，按架构校验 sha256。
# 这样 feed 的 commit 就同时固定了打包脚本与二进制版本，不需要在别处再钉一次。
LOCAL_BINARY:=$(wildcard ./files/aether-dns)
ifeq ($(LOCAL_BINARY),)
PKG_SOURCE:=aether-dns_$(PKG_VERSION)-r$(PKG_RELEASE)_$(ARCH).ipk
PKG_SOURCE_URL:=https://github.com/leochien0102/aether-dns/releases/download/v$(PKG_VERSION)/
ifeq ($(ARCH),aarch64_generic)
PKG_HASH:=7b2a2bebb82b2ea30543f48252a74d6fa921121c11b152fd1fffcc67d2cb37ad
else ifeq ($(ARCH),x86_64)
PKG_HASH:=504a64d0dec74917dbeca090a6bb522c4e5052a01e8cbc018e2fed15eaa1a511
else
# 其它架构由 DEPENDS 挡在门外；留空哈希让构建阶段报错，避免解析阶段中断 feed 索引。
PKG_HASH:=
endif
endif

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

ifeq ($(LOCAL_BINARY),)
define Build/Prepare
	$(INSTALL_DIR) $(PKG_BUILD_DIR)
	tar -xzf $(DL_DIR)/$(PKG_SOURCE) -C $(PKG_BUILD_DIR)
	tar -xzf $(PKG_BUILD_DIR)/data.tar.gz -C $(PKG_BUILD_DIR)
	mv $(PKG_BUILD_DIR)/usr/bin/aether-dns $(PKG_BUILD_DIR)/aether-dns
endef
else
define Build/Prepare
	$(INSTALL_DIR) $(PKG_BUILD_DIR)
	$(CP) ./files/aether-dns $(PKG_BUILD_DIR)/aether-dns
endef
endif

define Build/Compile
endef

define Package/aether-dns/install
	$(INSTALL_DIR) $(1)/usr/bin
	$(INSTALL_BIN) $(PKG_BUILD_DIR)/aether-dns $(1)/usr/bin/aether-dns
endef

$(eval $(call BuildPackage,aether-dns))
