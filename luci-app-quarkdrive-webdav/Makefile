# SPDX-License-Identifier: Apache-2.0

include $(TOPDIR)/rules.mk

PKG_NAME:=luci-app-quarkdrive-webdav
PKG_VERSION:=1.0.0
PKG_RELEASE:=2
PKG_LICENSE:=Apache-2.0

LUCI_TITLE:=LuCI support for QuarkDrive
LUCI_PKGARCH:=all
LUCI_DEPENDS:=+luci-base +rpcd-mod-file +ca-bundle

# The service configuration belongs to LuCI so a separately deployed binary
# can be used without installing the quarkdrive-webdav package.
define Package/luci-app-quarkdrive-webdav/conffiles
/etc/config/quarkdrive-webdav
endef

include $(TOPDIR)/feeds/luci/luci.mk

# call BuildPackage - OpenWrt buildroot signature
