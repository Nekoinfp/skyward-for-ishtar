# 压缩快照（VABC）支持：现状与方案

本文说明"在 recovery 里应用压缩快照/增量 OTA"这件事，**这台手机已经有什么、缺什么、为什么现在没做、要做需要哪些前提**。
所有"实测"均指对 OS4.0.4.0 底包分区的拆解，不含真机运行。

## 一、实测到的现状

| 项 | 结果 | 怎么查的 |
| --- | --- | --- |
| 内核是否支持 `dm-user` | **支持，已编入内核**（不是模块） | 从底包取 `boot` 分区，解出 36,866,560 字节的内核，里面有 `drivers/md/dm-user.c`、`dm_user_init`、`dm_user_message`、`dm_user.dm_user_daemon_timeout_msec`、`dm-user/%s` |
| 内核的 `dm-snapshot` | 有 | 同上，内核字符串里有 |
| `dm-user` 的设备名 | `/dev/dm-user/<name>` | 内核里 `dm-user/%s`；`snapuserd` 里 `/dev/dm-user/` |
| 系统的 `snapuserd` | **有，静态链接**（AArch64，无 INTERP、无动态段，1,273,320 字节） | 从 `system` 分区取出，`readelf` 查 |
| 它来自哪个源码树 | `system/fs/fs_mgr/libsnapshot/snapuserd/user-space-merge/` | 二进制的字符串常量 |
| 它认的属性 | `ro.virtual_ab.userspace.snapshots.enabled` | 同上 |
| recovery ramdisk 里有没有 | **都没有**（没有 `snapuserd`，也没有 `libsnapshot`） | 解出底包 recovery 的 ramdisk 查 |
| 原厂 recovery 的 `update_engine_sideload` | 在 recovery 中把 VABC 判为 NONE，增量包在 `DeleteSourcePartitions()` 处报错 | **不是本次实测**：来自 AOSP 源码与上游 issue 的记述，本机没有 AOSP 源码树可核对 |

结论：**"手机不支持"这个说法不成立**。内核和守护进程都在，缺的是 **recovery 侧的调度方**——也就是 libsnapshot 那半边的控制逻辑。

## 二、为什么现在没有做

不是能力不够，而是**在没有真机的情况下写它不负责**：

1. 它要动的是 `/metadata/snapshots/*.proto` 与 COW 镜像。**协议细节写错、顺序错、掉电时机错，都会让系统变成半合并状态**，而这类损坏不是"重刷 super"就能恢复的，可能连 `/data` 的密钥状态都要重来。
2. 内核 `dm-user` 的消息协议与 `snapuserd` 的 socket 协议必须**逐字节对上**。这两边都没有公开的稳定文档，只能对着 AOSP 源码与固件二进制反推——而且反推错了要真机才能发现。
3. 现有的**安全行为已经是对的**：遇到未合并快照，安装第二系统、OTA、刷 super、逻辑扩容四处都会拒绝（`Refusal::Pending`），提示用户重启到系统让它自己合并。也就是说，"不支持"并不会造成数据损坏。

## 三、要做的话，分四步

每一步都**独立可验证**，且失败不会破坏数据。

### 第 1 步：只读地读懂快照状态（风险最低）

- 读 `/metadata/snapshots/<slot>.proto`，解析出：哪些分区被快照、COW 设备、合并状态。
- 现状：`skyward-vab` 已经能告诉你"有没有更新在飞"（`snapshot_state`），但**没有解析 proto 内容**。
- 验证：在真机上把 proto 读出来，与 `skyward check` 的输出对照；不需要任何写操作。

### 第 2 步：把 `snapuserd` 运行起来（只读映射）

- 把系统的静态 `snapuserd` 复制进 recovery（或直接从挂载的 `/mnt/system` 执行）。
- 建一个 **dm-user** 设备（不是 dm-linear）指向它，读一次数据，确认内核与守护进程的握手成立。
- 验证：能通过 dm-user 设备读出快照分区的正确内容；**不写入任何东西**。

### 第 3 步：应用 `SOURCE_COPY` 类的增量包

- 只支持最简单的一类操作（整块拷贝），走 COW，不碰压缩。
- 验证：装完后 `dm-verity` 校验通过、能开机；失败则回滚（COW 丢掉即可，源分区没动）。

### 第 4 步：压缩操作与两阶段合并

- `XZ`/`ZSTD` 解压操作、两阶段合并、`snapuserd` 的合并模式、完成后把 proto 标记为 merged。
- 验证：合并后再刷一次完整包，确认状态自洽。

## 四、需要哪些前提才能开始

按必须程度排序：

1. **一台已解锁的 xuanyuan**——第 1 步起就需要真机，第 2 步之后每一步都必须真机验证；没有它就只能停在"读代码"。
2. **该固件的 libsnapshot 版本**：系统的 `libsnapshot.so` 在 `system` 分区里，字符串里能看出 proto 字段名与版本；需要与 AOSP 对应分支对照。
3. **AOSP 源码树**（`system/core/fs_mgr/libsnapshot`、`snapshot.cpp`、`snapuserd` 的 socket 协议）：用于对照协议，而不是靠猜。

## 五、与"非 A/B 更新包"的关系

同一节里另有一项"非 A/B 机型的更新包"。xuanyuan 是 A/B 机型，这类包在它上面永远用不到；
已有的安全行为是：包若声明了别的机型就拒绝（`declares_other_device`）。
除非目标改成"让 Skyward 支持某台非 A/B 机型"，否则这一项没有可做的内容。
