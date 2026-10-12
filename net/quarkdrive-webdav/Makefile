# SPDX-License-Identifier: MIT

include $(TOPDIR)/rules.mk

PKG_NAME:=quarkdrive-webdav
PKG_VERSION:=1.3.9
PKG_RELEASE:=1

# Match the source revision in feed/quarkdrive-webdav; pin it for reproducibility.
PKG_SOURCE_VERSION:=7a88d7f1fa299a6ef75b99d7e5dbc07a717f579e
PKG_SOURCE:=$(PKG_NAME)-$(PKG_VERSION).tar.gz
PKG_SOURCE_URL:=https://codeload.github.com/chenqimiao/quarkdrive-webdav/tar.gz/$(PKG_SOURCE_VERSION)?
PKG_HASH:=6e6457ae3459c41acb112d1848c1d690eca50168da84b17d3eb5bf0143f289df
PKG_BUILD_DIR:=$(BUILD_DIR)/$(PKG_NAME)-$(PKG_SOURCE_VERSION)

PKG_MAINTAINER:=Qimiao Chen <chenqimiao1994@126.com>
PKG_LICENSE:=MIT
PKG_LICENSE_FILES:=LICENSE
PKG_BUILD_DEPENDS:=rust/host
PKG_BUILD_PARALLEL:=1

include $(INCLUDE_DIR)/package.mk
include $(TOPDIR)/feeds/packages/lang/rust/rust-package.mk

# Cargo downloads also need bounded retries on unreliable networks.
CARGO_PKG_VARS += CARGO_NET_RETRY=5 CARGO_HTTP_TIMEOUT=60 CARGO_HTTP_MULTIPLEXING=false

define Package/quarkdrive-webdav
  SECTION:=net
  CATEGORY:=Network
  SUBMENU:=Web Servers/Proxies
  TITLE:=WebDAV server for QuarkDrive
  URL:=https://github.com/chenqimiao/quarkdrive-webdav
  DEPENDS:=$(RUST_ARCH_DEPENDS) +ca-bundle
endef

define Package/quarkdrive-webdav/description
  A WebDAV server for QuarkDrive. The binary is installed in /usr/bin.
  Service configuration and the procd init script are provided by the optional
  luci-app-quarkdrive-webdav package.
endef

define Package/quarkdrive-webdav/install
	$(INSTALL_DIR) $(1)/usr/bin
	$(INSTALL_BIN) $(PKG_INSTALL_DIR)/bin/quarkdrive-webdav $(1)/usr/bin/
endef

$(eval $(call RustBinPackage,quarkdrive-webdav))
$(eval $(call BuildPackage,quarkdrive-webdav))
