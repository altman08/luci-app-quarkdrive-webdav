# AN7581 EIP93 TLS 加速

`patches/100-an7581-eip93-rustls.patch` 在固定上游源码上增加 rustls
CryptoProvider，覆盖 TLS 1.2/1.3 的 AES-128-GCM 和 AES-256-GCM。
在两个 reqwest 客户端创建之前注册，对夸克 API、HTTPS 下载和上传连接生效。
不修改共享 Rust 工具链，也不依赖 OpenSSL。

## 激活范围

- 仅 Linux ARM64，根设备树 `compatible` 必须完整匹配
  `airoha,an7581` 或 `airoha,en7581`。
- 排除 AN7583/EN7583 和支持 CPU AES 指令的设备；其他设备沿用原版 ring。
- 仅卸载 AES-GCM 的记录加解密，8–32 KiB 载荷、12 字节 nonce、16 字节 tag、
  AAD 不超过 256 字节。两种接口、AES-128/256、加密/解密共用 8192 字节下限；
  较小的记录继续使用软件。阈值按明文记录长度计算，不含 tag 和 TLS 1.2 显式 nonce；
  TLS 1.3 的记录内层类型字节计入长度。
- EIP93 执行 AES-CTR，GHASH 由内核 CPU 实现。这是混合加速，不是硬件原生 GCM。
- 保留原有密码套件顺序；协商 ChaCha20-Poly1305 的连接不使用 EIP93。
- WebDAV 入站仍为 HTTP。302 后由客户端直接下载的文件流量不经过该进程。

## 后端与配置

默认 `auto`：`/dev/crypto` → AF_ALG → ring 软件实现。
`devcrypto`、`afalg` 指定单一内核接口，不可用时退回软件；`off` 禁用加速。
每个方向/密钥复用自己的会话；失败后不在每条记录上反复探测。

LuCI 页面、UCI 配置和服务启动脚本均不参与 EIP93 判断，也不传递加速环境变量。
后端自行识别设备并自动选择接口；旧配置中遗留的 `eip93` 字段会被忽略。
直接运行后端时，仍可用 `QUARK_EIP93=auto|devcrypto|afalg|off` 做诊断，
该变量不会绕过设备与 CPU 能力检查。

每个方向/密钥复用私有输入、输出和内核会话缓冲区，AF_ALG 同时复用控制缓冲区。
TLS AAD 使用栈内存，加密的明文暂存和内核结果缓冲区在首次符合阈值时分配，之后复用。
每条记录只清零本次使用的区域，密钥/会话释放时再清理完整缓冲区。
认证成功后才复制结果；rustls 返回的出站 TLS 帧仍需要独立分配。

8 KiB 下限以降低 CPU 消耗为优先，来自本设备的 0.2.0 独立基准结果。
AES-128 在 8 KiB 可能略降吞吐，AES-256 更容易同时获得吞吐和 CPU 收益。
实际应用的收益还受 TLS 协商、记录大小、并发和网络条件影响。

AN7581 目标的包依赖自动选择 `kmod-crypto-hw-eip93`、`kmod-crypto-gcm`、
`kmod-crypto-user` 和 `kmod-cryptodev`，其他目标不会添加这些运行依赖。
按架构发布的通用 SDK 仍可构建二进制；安装到 AN7581 时应自行确认设备内核模块齐全。
通用 SDK 不能替代与设备内核版本匹配的 kmod 构建。

## 正确性与回退

cryptodev 使用 `CIOCGSESSINFO` 核对实际算法为 `gcm(aes)`、驱动为
`gcm_base(ctr(aes-eip93),ghash-generic)`；AF_ALG 精确绑定该驱动。
每个新会话先使用 8192 字节数据与 ring 比较加密和解密结果，并确认拒绝坏标签。
共享限制为最多 128 个内核会话，使用 RAII 释放。

内核输出放入私有缓冲区，认证成功后才交给 rustls；失败不修改调用者输入。
认证错误直接返回 TLS 解密失败，不切换后端重新接受数据。
普通运行错误按策略退回其他接口或软件；密钥字节和临时明文在释放时清零。
AF_ALG 设置一秒发送/接收超时。同步 cryptodev ioctl 及卡死的硬件 DMA 不受 socket 超时控制。

首次成功自检后，系统日志会包含以下消息之一：

```text
Quark EIP93: AES-GCM hybrid backend enabled (devcrypto CTR + CPU GHASH)
Quark EIP93: AES-GCM hybrid backend enabled (AF_ALG CTR + CPU GHASH)
```

## 验证状态与设备验收

主机已完成：应用 `cargo check`；11 项 EIP93 测试；显式运行的真实 AF_ALG
内核测试；15 项启动脚本测试。TLS 测试涵盖全部六个 AES-GCM 套件，与原版 ring
双向互通，混合记录大小、多记录传输、TLS 1.3 密钥更新和后端运行失败回退。
AF_ALG 主机测试显式使用内核通用软件驱动，另覆盖并发、错误 tag/AAD/nonce。
cryptodev 的 ioctl 编组、结果长度、缓冲区复用/清零、驱动筛选及会话回收由 UAPI 模型测试。
另验证两种接口的 8191/8192 字节边界，以及变长记录的结果缓冲区复用和认证失败清零。

这些不是 EIP93 硬件实测。尚未完成完整 OpenWrt SDK 打包、设备硬件互通、
长时间负载和吞吐/CPU 对比。

2026-11-02 的 `aarch64_generic-25.12.5` 流水线发现三处 musl/glibc 类型差异：
`ioctl` 的请求号，以及 `cmsghdr.cmsg_len`、`msghdr.msg_controllen`。
已改为按目标 libc 类型转换，保留 ioctl 请求号的位模式。修复后的补丁已从固定上游版本
重新应用，并通过 11 项主机 EIP93 测试及显式 AF_ALG 内核互通测试。
使用与报错流水线相同的 Rust 1.94.0，已完成 `aarch64-unknown-linux-musl`
release 编译及链接，生成静态 ARM64 ELF。此项使用 Zig 0.13.0 交叉工具链，
验证 Rust/musl 编译和链接，不等同于 OpenWrt SDK 打包通过。
同版本 Rust 的 x86_64 musl release 测试亦通过：11 项 EIP93 测试，以及显式运行的
真实 AF_ALG 内核互通测试（1 项）。

在应用补丁后的源码目录，可以运行：

```sh
cargo test --locked eip93
# 仅在提供通用 AF_ALG GCM 算法的 Linux 开发主机上：
cargo test --locked kernel_afalg_tls_interoperability -- --ignored --nocapture
```

`src/eip93/testdata/` 的证书/私钥仅用于本地测试，不编入发行二进制，不用于实际服务。
硬件测试可在 AN7581 上运行（或交叉构建测试程序后复制到设备），分别指定接口：

```sh
QUARK_EIP93=devcrypto cargo test --locked hardware_tls_interoperability -- --ignored --nocapture
QUARK_EIP93=afalg cargo test --locked hardware_tls_interoperability -- --ignored --nocapture
```

实际使用时对比 `auto` 与 `off` 的 HTTPS 下载、上传，核对文件哈希、吞吐及整机 CPU。
同时观察 `/proc/interrupts` 中 `1fb70000.crypto` 的增量，避免其他加密任务干扰。
激活日志本身不能证明实际文件流量经过硬件，也不能证明性能提升。
