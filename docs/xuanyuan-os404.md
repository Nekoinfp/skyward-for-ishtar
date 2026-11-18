# 小米 15 Ultra（`xuanyuan`）与澎湃 OS 4.0.4 适配说明

这份说明写的是本分支相对上游 `kirklin/skyward` 的改动、已经验证到什么程度、
以及哪些事还没有做。**没有真机验证的部分一律不算完成。**

## 一、`/data` 解密

底包与解密链来自 `zhouzt0121` 的 `origin/os404-adaptation` 分支，本分支另外做了两件事：

1. **ST54L 安全元件节点的属主与等待**（`devices/xuanyuan.toml`、`crates/skyward/src/main.rs`）
   - `stm_st54se_gpio` 驱动在 `finit_module` 返回之后才异步创建 `/dev/st54spi_gpio`。
     Android 17 上这个 probe 比模块加载慢，于是 `chown` 会以 `ENOENT` 失败，
     而旧代码一失败就中断整条密钥启动流程，`keymint` / `gatekeeper` 根本不会启动。
   - 现在 `hand_over` 对每个待交接节点最多等 5 秒（`wait_for_file`），等到再改属主与权限。
   - 设备配置里给 `/dev/st54spi_gpio` 加了 `[[owners]]`（`system:system 0660`），
     与 OS4 上 `android.hardware.secure_element-service.qti` 的运行账号一致。
     只改这一个节点，不做全局放宽。

2. **Binder 调用超时**（`crates/skyward-binder/src/driver.rs`）
   - 安全服务在拿不到安全元件时会一直不回答，驱动默认要等几十秒，界面就卡在解密页。
   - 移植 `transact_within`：在写端之外用 `poll` 等驱动的可读性，weaver 调用给 4 秒上限。
   - 与本分支已有结构（`Waiting` / `exchange` / `take_in`）合并：只有在**没有待告知驱动内容**
   时才先等可读，否则会与“先回执才能继续”的调用互相等死。

上游 issue #64 记录的直接原因是 `keymint` 缺 `libvndksupport.so`；适配分支已经通过
`provide_system_libraries()` 从已安装系统补 `libvndksupport.so` 与 `libdl_android.so`，
并给服务补 `/vendor/lib64/hw` 等库路径。本分支沿用该做法，没有扩大范围。

## 二、逻辑分区扩容（需要改 super 元数据）

`crates/skyward-lp`：

- `encode_metadata` 按**源元数据自己的版本**写回（1.0/1.1 的 128 字节头、1.2 的 256 字节头），
  retrofit 的 `_a` / `_b` 名称与 `ATTR_SLOT_SUFFIXED` / `GROUP_SLOT_SUFFIXED` /
  `BLOCK_DEVICE_SLOT_SUFFIXED` 标志在解析时被剥掉，编码时重新加回。
- `commit` 写目标 metadata slot 的**主备两份**，每份 `sync_data` 后回读比对，最后 `sync_all`。
- `grow` / `grow_avoiding`：只追加新的 `Linear` extent，从不移动、改写既有 extent；
  按设备 `alignment` / `alignment_offset` 对齐，跳过元数据区与已占用区间，
  检查组上限、设备边界、分区之间不重叠。

**一个必须说明的正确性点**：super 里同时放两个槽位的分区，而每个槽位的元数据只描述自己那份。
于是单看一份元数据去扩容，新 extent 可能正好压在另一槽位的分区上。
`grow_avoiding` 允许把“别人占用的扇区”传进来一起避开；`grow_across` 把多个槽位放在一起规划：
每个槽位都避开其它槽位的既有 extent 与之前已生成的计划，最后再整体校验一遍。
双槽规划放在 `skyward-lp` 里而不是安装流程里，就是为了能在电脑上测。

安装流程（`crates/skyward-install/src/run.rs::flash_logical`）：

1. 先算稀疏镜像展开后的大小；
2. 对所有选中的槽位**逐一规划**（不写盘）；
3. 合并校验两槽不重叠；
4. `detach_once` 后卸载各槽位的映射；
5. 提交元数据；
6. 重新映射为可写的 `skyward-flash-<base><suffix>`；
7. 写入并 `fsync`；
8. 卸载映射。

镜像大于分区时不再直接拒绝，而是按上面的流程用 super 的**空闲空间**扩容；
没有空闲空间或与另一槽冲突时，在写第一块盘之前就失败。

写入时“这是分区镜像还是文件系统镜像”是**显式传入**的（`image::Purpose`），
不是从设备路径猜的：逻辑分区映射出来的节点是 `/dev/block/dm-N`，
按名字判断会失效。这个区分决定两件事——签名镜像的 AVB footer 要不要搬到分区末尾，
以及写之前能不能先 discard 这一段。

## 三、sparse super 的并行写入

`crates/skyward-install/src/sparse.rs`（新文件）：

- **先完整预检再开设备**：`plan()` 校验魔数、版本、头长度、块大小、
  每个 chunk 的 `total` 与 payload 长度、`total_blocks` 与展开偏移、
  尾部多余字节。任何一处不对都在写盘前失败。
- **DON'T CARE 一律保留**：只 `seek` 过去，不清零、不 discard。
  这是 sparse super 的关键——没被 chunk 覆盖的区域属于 super 里其它分区（包括另一槽位）。
  因此这条路径**绝不**做整盘 discard。
- **有界流水线**：4 个 4 MiB 缓冲、一个读线程预读 RAW/FILL、主线程写；
  跨缓冲区的大 chunk 按实际偏移续读，FILL 的 4 字节 pattern 按 `consumed % 4` 续相位。
- **CRC32 记录按字节校验**：DON'T CARE 区间以零参与 CRC（与 AOSP 语义一致），
  但**不**写到设备上。CRC 不符会让整次写入失败。
- 写完 `sync_all`。

正确性上曾经踩到的坑（留在这里免得再犯）：
读线程在 CRC 不符时会提前返回，于是空闲缓冲通道被关闭；写线程回收缓冲失败后
直接 `break`，把“失败”走成了 `Ok(())`。现在回收失败不再结束循环，
已排队的数据照常取完并判定。

识别上，sparse 镜像按**展开后**的内容分类：展开到 `SUPER_GEOMETRY_AT` 处是
super 的 geometry 魔数就是 `super`，否则是逻辑分区文件系统镜像。
这个判定放在 `sparse::expands_to`，因为它只读覆盖该偏移的那一个 chunk 的四字节，
不必展开整个镜像，也就能在电脑上单独测。
erofs/f2fs 的超级块在偏移 1024，ext4 的魔数在 1080。

## 四、验证到什么程度

已经跑过、可以复现的：

| 范围 | 命令 | 结果 |
| --- | --- | --- |
| LP 读取/编码/扩容/提交 | `cargo test -p skyward-lp` | 21 项通过（原有 7 项、扩容与双槽规划 14 项） |
| sparse 解析与写入 | 独立 harness | 6 项通过（RAW/FILL/DON'T CARE 混合、坏 CRC 拒绝、格式错误不改盘、多段空洞、super 判定、头扩展与尾部填充） |
| Android 全工作区 | `cargo ndk -t arm64-v8a --platform 30 check --workspace --all-targets` | 通过 |
| Android 全工作区 lint | `cargo ndk -t arm64-v8a --platform 30 clippy --workspace --all-targets -- -D warnings` | 通过，零告警 |

上面四项都是在提交 `07bec16` 的干净工作树上跑的（`cargo` 退出码 0）。
Windows 主机上跑不了整个 workspace 的 `cargo test`：`skyward-bootimg` 的 ramdisk 解包用的是
Unix 专有接口，这与本次改动无关。CI 的 `ubuntu-24.04` 上会跑完整 `cargo test --workspace`。

另外两项构建输入的核对：

| 项 | 做法 | 结果 |
| --- | --- | --- |
| `devices/xuanyuan.toml` 的 `[device] ab` 等配置能被解析 | `cargo test -p skyward-config` | 28 项通过 |
| `[stock] recovery` 的 SHA-256 与固件一致 | 从 `payload.bin` 清单里读 recovery 分区的声明哈希 | 一致（`55f49105…7fea27b`） |

第二项是这样做的：底包 zip 有 9.7 GB，但 `payload.bin` 以 Stored 方式存放，
所以按 HTTP Range 只读了它的清单（约 370 KB），解析 protobuf 取出
`partitions[name="recovery"].new_partition_info.hash` 与配置比对。
底包地址本身返回 `200`、`Content-Length` 10382731686 字节。

**没有验证的**：真机上的 `/data` 解密、OTA 安装、扩容后 mount、sparse 实际写盘。
这些都需要一台已解锁的 xuanyuan 配合原厂 OS4.0.4.0 底包，本环境做不到。
所以本分支**不宣称**“已完全适配”，只提供可编译、单元测试通过、且失败会安全中止的实现。

## 五、明确没有做的部分

1. **recovery 内应用/合并压缩快照（VABC）**
   原厂 recovery 版 `update_engine_sideload` 在 recovery 中直接把
   `GetVirtualAbCompressionFeatureFlag()` 判为 NONE（AOSP 注释就写着
   “Don't attempt VABC in recovery”），增量 payload 还会在 `DeleteSourcePartitions()`
   处直接报错。也就是说：**打开包检查的开关并不能让压缩快照工作**。
   要在 recovery 内做这件事，需要与固件匹配的 `libsnapshot` / `snapuserd.recovery`、
   `dm-user` 设备表与两阶段合并，属于另一次 AOSP 侧的工作量，不是几十行 Rust 能补的。
   因此这里保持**显式拒绝**（`Refusal::Incremental`，以及 `snapshots.rs` 对
   `using_snapuserd` 的 `ensure!`），失败原因写在日志里，不做半吊子处理。

   顺带说明现有能力边界：**完整包（非增量）走原厂引擎在 recovery 内写的是内核
   `dm-snapshot` 快照**，这条路径 Skyward 本来就能映射与合并，压缩快照这一缺口不影响它。

2. **非 A/B 机型的更新包**
   `xuanyuan` 是 A/B + 动态分区机型，这条对本案没有实际影响。
   唯一相关的安全口子是要保证 A/B 机型上不会误跑 BLOCK 包，现有 `package::check`
   的 `NotAb` 拒绝保持不变。

3. **真机回归**：见下。

## 六、构建

与上游一致，用 Linux 或 WSL（Windows 上 `cargo ndk` 需要 NDK + 可用的 host 链接器）：

```sh
cargo ndk -t arm64-v8a --platform 30 build --release -p skyward
RUSTFLAGS="-C target-feature=+crt-static" \
  cargo ndk -t arm64-v8a --platform 30 build --release \
  -p skyward-system-init -p skyward-permissive-init --target-dir target/system-init
cargo build --release -p skyward-pack

target/release/skyward-pack stock --config devices/xuanyuan.toml --out stock/recovery.img
target/release/skyward-pack recovery \
  --stock stock/recovery.img \
  --binary target/aarch64-linux-android/release/skyward \
  --system-init target/system-init/aarch64-linux-android/release/system-init \
  --permissive-init target/system-init/aarch64-linux-android/release/permissive-init \
  --config devices/xuanyuan.toml \
  --font resources/fonts/MiSansVF.ttf \
  --mono resources/fonts/KirkMono-Regular.ttf \
  --mono-license resources/fonts/KirkMono-OFL.txt \
  --omapi resources/omapi \
  --out out/skyward-xuanyuan-OS4.0.4.0.img
```

`.github/workflows/build-xuanyuan.yml` 已经按这套流程构建并校验原厂 recovery 的 SHA-256。

## 七、真机验证清单

按顺序做，每一步失败就停在那里，不要继续：

1. **备份**（在已 root 的系统或 fastboot 里）
   ```sh
   dd if=/dev/block/by-name/recovery_a of=/sdcard/recovery_a_stock.img
   dd if=/dev/block/by-name/recovery_b of=/sdcard/recovery_b_stock.img
   dd if=/dev/block/by-name/super        of=/sdcard/super_stock.img   # 扩容前必须
   ```
2. **刷入** `recovery_a` / `recovery_b`，`fastboot reboot recovery`。
3. **解密**：进 recovery，看 `/skyward/run/recovery.log`
   - 期望 `keymint` / `gatekeeper` 注册，`/dev/block/dm-* /data f2fs rw`
   - 输锁屏密码后 `/sdcard` 可读
   - 若失败，先看是不是 `CANNOT LINK EXECUTABLE`（缺库）还是
     `the service refused the call`（安全元件/weaver），两者处理方式不同
4. **完整 OTA**：装一个 OS4.0.4.0 完整包，确认能写、能切槽、能起机。
5. **逻辑分区扩容**（可选，风险最高）：
   - 先准备一个比现有 `system` 大的逻辑分区镜像
   - 记录 `lpdump` 前后 result，确认只增加了本分区的 extent，另一槽未变
   - 刷完重启，确认能挂载
6. **sparse super**：准备原厂 `super.img`（sparse）刷入，确认重启后两槽分区都在。

第 5、6 步一旦中途断电，按厂商方式重刷完整 `super` 恢复；提交元数据本身不是掉电原子操作。
