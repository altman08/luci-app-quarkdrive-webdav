# EIP93 AES-GCM：Rust / Go 对比测试 0.2.0

两份独立 Linux ARM64 静态程序，使用相同 CLI、块大小、加解密流程、计时和 CPU 统计口径。
软件路径分别使用 ring 0.17.14 和未修改的 Go 1.27.2 标准库 AES-GCM，保留各自原生优化。

## 设备上对比

```sh
chmod +x eip93-bench-rust-aarch64-musl eip93-bench-go-aarch64
./eip93-bench-rust-aarch64-musl --csv /tmp/eip93-rust.csv
./eip93-bench-go-aarch64 --csv /tmp/eip93-go.csv
```

请按顺序运行，避免两份程序争抢 CPU / EIP93。每份默认约 3 分钟，建议空闲设备上运行，
再反转 Go/Rust 顺序复测，减小温度、CPU 调频和后台负载影响。

更短的典型 TLS 测试（每份约 1 分钟）：

```sh
./eip93-bench-rust-aarch64-musl --sizes 1024,4096,16384 --seconds 0.5 --repeats 3 --cpu 0 --csv /tmp/eip93-rust.csv
./eip93-bench-go-aarch64 --sizes 1024,4096,16384 --seconds 0.5 --repeats 3 --cpu 0 --csv /tmp/eip93-go.csv
```

`--cpu 0` 固定工作线程，不改变设备 IRQ 亲和性；两个程序应选相同 CPU。
Go 使用 GOMAXPROCS=1 并固定工作 goroutine 到 OS 线程；进程 CPU 包含 Go 运行时/GC。
Rust 使用单工作线程。两份程序都不主动修改 CPU 频率、加载模块或改系统配置。

默认参数：

- `--backend all`：software、devcrypto、afalg。也可单独指定其中一个。
- `--key-bits all`：AES-128-GCM 和 AES-256-GCM。也可指定 128 或 256。
- `--sizes 64,256,512,1024,1500,2048,4096,8192,16384,32768`：明文有效负载字节数。
- `--seconds 0.5 --repeats 3`：每组测量 0.5 秒，三次重复；显示各列中位数。
- `--csv PATH`：保存原始样本，每完成一个样本就刷新。
- `--help`：全部选项。采样时间范围 0.01..60 秒，建议正式比较用 1..2 秒。

复制两个 CSV 回开发电脑，可运行本目录的比较工具：

```sh
python3 compare.py eip93-rust.csv eip93-go.csv
```

比较软件的绝对吞吐，再比较同一语言内硬件相对软件的收益；两个程序各自的 `speedup`
基线不同。硬件绝对吞吐接近但 Go 的 speedup 更高，可能来自 Go 软件基线更低。

## 两个版本统一的数据处理流程

每条记录均为：复制输入到私有缓冲区 → 加解密 → 认证成功后复制到调用者缓冲区 →
清零私有缓冲区本次使用的区域 → 消费输出首末字节 → 清零调用者输出使用区域。

所有负载缓冲区、调用者输出、内核会话、密钥初始化均在计时前完成；软件路径也复用
私有和调用者缓冲区。AF_ALG 输入、输出、控制缓冲区复用；cryptodev 使用对齐私有缓冲区。
不再每条记录新建结果 Vec，不再为小记录清零整个 32 KiB 缓冲区。
基准自身的成功记录路径没有每条记录的堆分配；密码库和 Go 运行时的内部开销仍计入。

计时包含输入/输出复制、内核系统调用、实际使用区域的清零、nonce 更新和输出消费。
计时不包含会话创建、驱动检查、自检、文件写入和每组 25 ms 预热。每 8 条记录检查一次时钟。
每个样本生成新密钥；加密 nonce 持续递增，解密使用经自检确认的固定有效密文。

清零在两个程序中均使用不可内联的优化内存清零函数，Rust 使用 fill(0) + black_box，
Go 使用 clear + KeepAlive。Rust 的 Zeroizing 只在缓冲区释放时再次清理，释放不在计时内。
这是基准程序的数据处理契约；Go 编译器/运行时与 Rust 的具体指令不保证相同。

0.2.0 的软件路径也改成相同的输入/输出复制流程，因此不要把其吞吐与 0.1.0 混为同一基线。
WebDAV 软件包补丁已同步缓冲区复用及按使用长度清零，两种内核接口统一采用 8192 字节门槛。
应用仍需生成 rustls 的出站 TLS 帧，不能把基准的零分配结论直接套用到整个 TLS 路径。

## 驱动和正确性

`/dev/crypto` 使用 AEAD ioctl 并通过 CIOCGSESSINFO 核对名称 gcm(aes)、实际驱动
`gcm_base(ctr(aes-eip93),ghash-generic)`；AF_ALG 精确绑定同名驱动。
EIP93 做 AES-CTR，GHASH 在 CPU 上。接口缺失、选错驱动、数据不一致或运行错误会明确报错，
继续其他组并最终以非零状态退出；硬件测试绝不回退成软件结果。

使用 12 字节 nonce、16 字节 tag、5 字节 TLS 1.3 风格 AAD。每组先对比软件密文/明文，
验证坏 tag 必须返回 EBADMSG，错误不得改动调用者输出，并验证错误后的会话仍可使用。
程序绕过 WebDAV 的设备识别和 8192 字节门槛，以测试小块收支；最大负载仍为 32768 字节。
内核驱动自身仍可能对小请求使用软件回退，选中 EIP93 驱动不表示每条记录都执行 DMA。

开发主机可显式指定通用内核驱动验证 AF_ALG；该模式会打印诊断标记并在 CSV 记录驱动名：

```sh
./eip93-bench-rust-aarch64-musl --backend afalg --afalg-driver 'gcm_base(ctr(aes-generic),ghash-generic)'
./eip93-bench-go-aarch64 --backend afalg --afalg-driver 'gcm_base(ctr(aes-generic),ghash-generic)'
```

## 指标

| 列 | 含义 |
| --- | --- |
| MiB/s | 明文有效负载吞吐，不含 AAD、nonce、tag |
| ops/s、us/op | 每秒记录数、平均单条记录耗时 |
| user%、sys%、proc% | 进程用户态、内核态、总 CPU 时间 / 墙钟时间；100% = 一个逻辑 CPU |
| machine% | /proc/stat 所有 CPU 的总体忙碌率；100% = 整机满载；包含 IRQ、softIRQ 和其他任务 |
| cpu-ms/MiB | 每 MiB 消耗的进程 CPU 毫秒；越低越好，不含记账在进程外的中断等 |
| speedup | 相同语言、密钥、块大小、方向下，相对软件基线的吞吐倍率 |
| IRQ-delta | 单个样本的 EIP93 中断增量，中位数；计数器不可用时 n/a |
| n | 成功样本数；完整测试应等于 repeats |

同时看吞吐、proc%、machine% 和 cpu-ms/MiB。进程 CPU 较低可能来自等待硬件，中断开销
也可能记账在进程外；较低的 proc% 不能单独证明更快或整机总 CPU 更省。
IRQ 计数可能包含其他 EIP93 用户。短样本的整机统计受 tick 粒度影响；正式比较建议增加采样时间。
CSV 统一列名，包含 implementation、version、driver，保留原始样本供重新计算。

## 构建和验证

Rust 1.94.0、目标 aarch64-unknown-linux-musl，ring 需要交叉 C 编译器/linker/ar。
本次使用 Zig 0.13.0 的 aarch64-linux-musl 工具链，Rust 设置 -C link-self-contained=no。
Go 1.27.2 未使用 Xray overlay，CGO_ENABLED=0、GOOS=linux、GOARCH=arm64、GOARM64=v8.0。

```sh
cargo build --locked --release --target aarch64-unknown-linux-musl
cd go
CGO_ENABLED=0 GOOS=linux GOARCH=arm64 GOARM64=v8.0 go build -trimpath -ldflags='-s -w' -o eip93-bench-go-aarch64 .
```

主机检查包含已知答案、坏标签和输出保密、变长重复使用、cryptodev UAPI 模型/清零、
CPU 单位，以及真实通用 AF_ALG 的缓冲区复用与零堆分配检查。EIP93 真实性能需要在设备上测量。

2026-11-02 验证：Rust 5 项测试通过；Go 6 项测试通过，禁用 CPU 扩展后也通过。
Rust/Go 各 160 组真实软件/通用 AF_ALG 短样本通过，覆盖默认所有大小、AES-128/256、
加密/解密和两次重复。另核对 CSV 算式、缺失接口不回退、CPU 亲和性和 CSV 对比工具。
Go ARM64 vet 通过。两个发行文件均已核对为 AArch64 静态 ELF，无动态节和解释器。

```sh
cargo test --locked --release
# Linux 开发主机上显式运行通用 AF_ALG：
cargo test --locked --release afalg_reuse_authentication_and_no_allocations -- --ignored
cd go
EIP93_TEST_AFALG=1 go test -v .
```

主程序和测试 MIT；内核接口实现 BSD-3-Clause，见 LICENSE、LICENSE.xray。
