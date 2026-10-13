# luci-app-quarkdrive-webdav

服务端项目：[chenqimiao/quarkdrive-webdav](https://github.com/chenqimiao/quarkdrive-webdav)。感谢作者 chenqimiao 的开源分享。

本项目是面向 OpenWrt / ImmortalWrt 的夸克网盘 LuCI 控制插件，用于配置和管理 `quarkdrive-webdav` 服务。

## 软件包与功能

| 软件包 | 用途 |
| --- | --- |
| `luci-app-quarkdrive-webdav` | LuCI 页面、UCI 配置、procd 启动脚本和 RPC 权限 |
| `luci-i18n-quarkdrive-webdav-zh-cn` | 简体中文翻译 |
| `quarkdrive-webdav` | Rust 后端二进制，安装到 `/usr/bin/quarkdrive-webdav` |

LuCI 包**不依赖后端包**。可以安装本仓库编译的后端，也可以自行部署兼容的二进制程序，然后在界面中指定绝对路径。

支持启用服务、设置 Cookie、WebDAV 身份验证、监听地址与端口、网盘根目录、只读模式、缓存及缓冲区、上传等待时间、URL 前缀、重定向和调试日志，并提供状态显示、启动/停止/重启和最近服务日志。

## 安装与使用

1. 安装与系统版本、包管理器及 CPU 架构匹配的软件包。使用本仓库后端时安装上述三个包；自行部署后端时只需安装 LuCI 和中文翻译包，依赖由包管理器处理。
2. 打开 LuCI 的“服务 → 夸克网盘 WebDAV”。
3. 设置“二进制路径”，默认 `/usr/bin/quarkdrive-webdav`。自定义程序例如放在 `/mnt/sda1/quarkdrive-webdav`，必须有执行权限，所在文件系统也必须允许执行。
4. 填写完整的夸克网盘 Cookie，以及所需的 WebDAV 用户名和密码。
5. 在“状态与操作”中勾选“启用”，再点击“保存并应用”，即可启动服务；取消启用后保存并应用则停止服务。

自定义二进制应适配路由器的架构、ABI 和运行库，并支持本插件使用的命令行选项。插件不会自动下载或升级二进制。

默认监听 `0.0.0.0:8080`，网盘根目录为 `/`。根目录指的是**夸克网盘中的目录**，不是本地挂载点。WebDAV 客户端连接地址为 `http://<路由器地址>:8080/`，使用其他端口时相应修改。

配置文件为 `/etc/config/quarkdrive-webdav`，服务默认关闭。安装时会启用开机启动脚本，普通启动和开机启动均遵循配置中的 `enabled`；启用且二进制、Cookie 等配置有效时才会启动进程。关闭 LuCI 页面不会停止服务。

页面分为“状态与操作”“基本设置”“高级设置”和“服务日志”。“保存”暂存配置，不主动改变当前运行状态；“保存并应用”提交配置，启用时启动或重启服务，未启用时停止服务。应用时会先验证需要启动的配置，再停止旧进程，避免明显的配置错误中断现有服务。

“启动 / 停止 / 重启”执行临时操作，使用已应用的配置，不修改启用开关。未启用时也可以手动启动；启用时手动停止，下次应用配置或开机仍会启动。修改路径、Cookie 等配置后应先保存并应用，再进行手动操作。

| 已保存的启用开关 | 实际运行状态 | 页面显示 | 可用服务按钮 |
| --- | --- | --- | --- |
| 关闭 | 停止 | 未启用，服务当前未运行 | 启动 |
| 开启 | 停止 | 已停止 | 启动 |
| 开启 | 运行 | 运行中，并显示 PID | 停止、重启 |
| 关闭 | 运行 | 运行中（未启用），并显示 PID | 停止、重启 |

页面每 5 秒读取服务状态和已保存的启用值，尚未保存的勾选不会改变状态标签。启动 / 停止 / 重启后局部刷新，保留尚未保存的表单内容；操作期间暂停轮询并禁止重复操作。状态读取失败和只读账号会禁用服务控制；“打开”按钮在服务运行时可用。运行状态来自 procd，表示进程正在运行，不代表 Cookie 或 WebDAV 访问已经验证成功。

启动命令等待 procd 报告运行，停止命令等待旧进程退出及实例清理；停止已经停止的服务也视为成功。临时服务管理命令：

```sh
/etc/init.d/quarkdrive-webdav start_now
/etc/init.d/quarkdrive-webdav stop_now
/etc/init.d/quarkdrive-webdav restart_now
/etc/init.d/quarkdrive-webdav log
```

普通 `start`、`restart` 和 `reload` 遵循 `enabled`；`reload` 根据启用开关启动 / 重启或停止服务。临时停止保留配置变更监听，之后应用新配置仍能触发服务重载。旧版 `enabled` 值直接沿用，现在同时影响开机启动和配置应用后的运行状态。

## Cookie 如何传入后端

**通过环境变量传入，而不是命令行参数。** 无需用户在终端中手动 `export`。

传递流程：

1. 在 LuCI 中填写“夸克网盘 Cookie”，保存到 UCI 的 `quarkdrive-webdav.config.quark_cookie`。
2. 启动脚本使用 `config_get` 读取该值。
3. procd 将其设置为后端进程的 `QUARK_COOKIE` 环境变量。
4. 后端通过命令行解析库的环境变量支持读取 `QUARK_COOKIE`。

WebDAV 用户名和密码也采用同样方式：

| UCI 配置项 | 后端环境变量 |
| --- | --- |
| `quark_cookie` | `QUARK_COOKIE` |
| `auth_user` | `WEBDAV_AUTH_USER` |
| `auth_password` | `WEBDAV_AUTH_PASSWORD` |

在已登录夸克网盘的浏览器中打开开发者工具，在网络请求中找到发往夸克网盘的请求，复制请求头 `Cookie` 的完整值，例如 `name=value; other=value`。不要复制 `Cookie:` 前缀，也不要将 `Set-Cookie` 响应头直接当作请求 Cookie。Cookie 失效后需要重新获取；服务保持启用时，保存并应用会自动重启服务。

环境变量传递**不等于加密存储**：Cookie 和密码以明文保存在路由器的 UCI 配置中，安装初始化时将配置权限设为 `0600`；root、获授权的管理用户及管理接口仍可能读取凭据或进程环境。请保护路由器管理权限、配置备份和日志，不要把真实 Cookie 提交到 GitHub。

## 手动作为 feed 编译

在已准备好编译依赖的 OpenWrt / ImmortalWrt 源码根目录或匹配设备的 SDK 中操作。需要标准 `packages`、`luci` feed；编译后端还需要提供 `rust/host` 和 `lang/rust/rust-package.mk` 的 packages feed。后端使用 Rust 2024 edition，要求 Rust 1.85 或更新版本，并以 `Cargo.lock` 锁定依赖。

在实际使用的 `feeds.conf` 或 `feeds.conf.default` 中加入以下条目，将路径替换为本仓库的绝对路径：

```text
src-link quarkdrive /path/to/luci-app-quarkdrive-webdav
```

路径应指向同时包含 `luci-app-quarkdrive-webdav/` 和 `net/` 的仓库根目录，不能让该目录包含正在使用的 SDK，以免扫描自身。远程仓库可改用 `src-git quarkdrive <仓库 Git URL>`；已有同名条目时修改原条目，不要重复追加。

更新并安装 feed：

```sh
./scripts/feeds update -a
./scripts/feeds install -f -p quarkdrive quarkdrive-webdav luci-app-quarkdrive-webdav
make menuconfig
```

`-f -p quarkdrive` 用于选择本仓库的包，避免其他 feed 中同名包覆盖本项目。通过菜单或编辑 `.config` 选择：

```text
CONFIG_PACKAGE_quarkdrive-webdav=m
CONFIG_PACKAGE_luci-app-quarkdrive-webdav=m
CONFIG_PACKAGE_luci-i18n-quarkdrive-webdav-zh-cn=m
CONFIG_LUCI_LANG_zh_Hans=y
```

已有同名配置项时修改原项。`m` 表示生成独立软件包，编入固件时改为 `y`；自行部署二进制时无需选择后端包。

```sh
make defconfig
make package/quarkdrive-webdav/compile V=s
make package/luci-app-quarkdrive-webdav/compile V=s
```

只编译界面时省略后端编译命令。中文翻译由 LuCI 构建流程作为子包生成，没有单独的源码编译目标。产物通常位于 `bin/packages/<arch>/quarkdrive/`，格式为 `.ipk` 或 `.apk`，由所用源码或 SDK 决定。

后端源码从上游 [chenqimiao/quarkdrive-webdav](https://github.com/chenqimiao/quarkdrive-webdav) 下载，版本及固定提交由 `net/quarkdrive-webdav/Makefile` 指定。旁边的本地源码目录不参与默认下载构建；修改本地源码不会自动改变 GitHub 构建结果。

## GitHub Actions 编译与发布

流水线参考 `luci-app-shellinabox`，使用 `openwrt/gh-action-sdk@v11`，SDK 版本为 `25.12.5`，保留以下六个架构：

- `x86_64`
- `aarch64_cortex-a53`
- `aarch64_cortex-a72`
- `aarch64_generic`
- `arm_cortex-a7`
- `arm_cortex-a9`

发布步骤：

1. 将本仓库内容放在 GitHub 仓库根目录，包含 `.github/`、`luci-app-quarkdrive-webdav/` 和 `net/`；保留 `po/zh_Hans` 翻译入口及其目标文件。
2. 确保工作流位于默认分支，在 Actions 中选择“Release OpenWrt packages → Run workflow”。
3. 填写发布标签，例如 `v1.0.0-test1`；首次试编译建议勾选 `prerelease`。
4. 每个架构必须生成后端、LuCI 和中文翻译三个包，缺失任一包会报错。全部成功后自动创建 Release 并上传安装包和 SHA256 校验文件。

流水线仅收集本项目三个包，不发布依赖包。文件名附带 SDK 版本和架构，避免通用 LuCI 包在不同任务间重名。失败时可下载构建日志。

构建任务超时为 90 分钟，SDK 构建步骤为 75 分钟；Release 查询、创建和上传保留参考流水线的三次重试，每次操作限制为 90 秒。Cargo 依赖下载另设置重试次数 `5`、HTTP 超时 `60` 秒并关闭 HTTP 多路复用。SDK Action 内部网络操作由上游实现管理，不能保证所有网络阶段都使用相同重试策略。

## 本地验证

在仓库根目录运行以下测试，需要 Node.js 18 或更新版本和 Python 3：

```sh
node --test tests/view.test.js
python3 tests/init.test.py
```

前端测试模拟 LuCI 接口，覆盖四种状态、按钮权限、临时服务命令、保存 / 应用、过期响应及未保存内容保留。启动脚本测试模拟 UCI / procd，并使用真实的延迟退出进程，覆盖启用语义、启动失败、配置校验、重复停止、退出等待、超时和重载监听保留，不操作实际服务。

设备验收应确认：四种状态及按钮显示正确；修改启用后应用可启动或停止；临时启停不修改启用值；手动停止后修改配置并应用仍能启动；缺少程序或 Cookie 时显示失败；只读账号不能控制服务。保存应用沿用 LuCI 的配置应用流程，没有配置变化时不会额外触发重载。

## 安全与兼容性

- 当前固定提交的后端仅监听 HTTP，虽然源码声明 TLS 参数，但未实际接入 TLS 监听；如需 HTTPS，请使用反向代理。
- WebDAV 用户名和密码必须同时填写或同时留空。留空会关闭认证，任何能够连接该端口的人都可能访问网盘文件。
- 不建议直接暴露到公网；请结合监听地址、防火墙、VPN 或反向代理限制访问。LuCI 登录认证不等于 WebDAV 身份验证。
- 调试日志可能包含敏感信息，排查后请关闭；服务日志写入系统日志，不保证重启后保留。
- SDK 包需要与设备的系统版本、包管理格式、架构和 ABI 兼容，不能只按 CPU 名称判断。实际交叉编译与路由器运行仍需验证。
