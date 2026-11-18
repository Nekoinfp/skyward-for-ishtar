# 压缩快照（snapuserd / VABC）：现状与方案

本文说明"压缩快照"这件事在这台手机上**已经做到了哪一步、缺的到底是什么、不做的风险在哪**。
"实测"指对本机取出的 OS4.0.4.0 分区与仓库代码的核对，不含真机运行。

> 上一版本文把缺口写成"缺 recovery 侧的控制逻辑"，读完 `crates/skyward-vab/src/snapshots.rs` 后
> 这个说法是错的，已按代码改写。

## 一、已经实现的（代码里就有）

`crates/skyward-vab/src/snapshots.rs`、`partitions.rs` 里已经有整套**内核 `dm-snapshot` 路线**：

| 能力 | 位置 |
| --- | --- |
| 读/写 `/metadata/snapshots` 的 `snapshot.proto` 元数据，`SnapshotStatus` 逐字段定义（含 `using_snapuserd`、`compression_algorithm`、`read_ahead_size`） | `SnapshotStatus`、`status()`、`update()`、`state()` |
| 把快照分区映射出来：`-base`（dm-linear 盖在 super 上）、COW（super 的 COW 分区 + 可选的 `/data` 上的 COW 镜像）、`dm::Target::Snapshot`（`snapshot` / `snapshot-merge`） | `map_snapshot()` |
| 驱动合并：两阶段顺序、进度、COW 用量、清理已被覆盖的槽的 COW | `initiate()`、`check()`、`progress()`、`Usage` |
| 判断"这槽在等合并 / 已经合并 / 还没写完" | `Standing`、`snapshot_state()` |

也就是说：**只要快照是内核 `dm-snapshot` 能服务的，Skyward 已经能读、能合、能看进度。**

## 二、明确没有实现的，只有一种：`using_snapuserd`

代码在三处**显式拒绝**，报的是同一句话：

```
the update's snapshots are compressed and served by snapuserd, which this recovery does not run
```

位置：`map_snapshot()`、`initiate()`，以及 `initiate()` 内逐分区的检查。

## 三、手机这一侧其实齐备（本轮实测）

| 项 | 结果 | 怎么查的 |
| --- | --- | --- |
| 内核 `dm-user` | **已编入内核**（不是模块）：`drivers/md/dm-user.c`、`dm_user_init`、`dm_user_message`、`dm_user.dm_user_daemon_timeout_msec` | 从底包取 `boot`，解出 36,866,560 字节内核，`strings` |
| `dm-user` 的设备名 | `/dev/dm-user/%s` | 同上；`snapuserd` 里也有 `/dev/dm-user/` |
| 系统的 `snapuserd` | **静态链接**（AArch64，无 INTERP、无动态段，1,273,320 字节），来自 `system/fs/fs_mgr/libsnapshot/snapuserd/user-space-merge/` | 从 `system` 分区取出，`readelf`、`strings` |
| recovery ramdisk 里 | 两者都**没有** | 解出底包 recovery 的 ramdisk |

所以缺的不是手机的能力，是**recovery 这一侧没有去用它们**。

## 四、这件事的实际影响（比"不能更新"更要紧）

如果手机上存在一个**未合并的压缩快照**，而 Skyward 又跑在这一槽上，那么 `mount_partitions()`
会在这里失败，结果是：

1. **这一槽的分区一个都挂不上**——不是只跳过快照那个分区：`mount_snapshot()` 的错会中止
   整个 `mount_partitions()`，循环里已经处理完的那些也不会交出去；
2. keymint、gatekeeper 这些程序在 `/vendor` 上，**因此起不来**；
3. `/data` 的元数据密钥解不开 → **数据分区保持锁定**。

也就是说：**一个挂着的压缩快照会让 recovery 读不了这一槽的系统，进而解不开 /data。**
这不是"能不能刷更新包"的问题，而是"recovery 还算不算可用"的问题。

现有的出路是对的：`prepare()` 把原因写进内核日志、界面照常显示，
`skyward check` 会说出是哪些服务没注册；界面文案也告诉用户"重启到系统让它合并"。

## 五、两条可选路线

### A. 实现 snapuserd 模式（完整做法）

- 把系统里的静态 `snapuserd` 取出来（复制进 recovery，或从挂载的 `/mnt/system` 直接执行）；
- 建 **dm-user** 设备（内核已支持），把 daemon 的 misc 设备作为 target；
- 按 `SnapshotStatus` 里已有的参数（压缩算法、read-ahead 等）启动 daemon 并建 socket；
- 合并模式下走 daemon 的用户态合并，而不是内核 `snapshot-merge`。

风险：内核 `dm-user` 的消息协议与 `snapuserd` 的 socket 协议必须逐字节对上；
**协议错一步，合并会写到错的地方**，而这种损坏不是重刷 super 就能恢复的。

### B. 只读回退：快照读不了时，退回快照的"基线"——**查过之后结论是：不该做**

想法是：未合并时 `-base`（dm-linear 盖在 super 上）就是更新前的内容，只映射它就能读到
`vendor`，keymint 起来，`/data` 也许就能解开。

**但这条不成立**，原因在 `crates/skyward-info/src/lib.rs::installed_props()`：
版本号与补丁级别是**从挂载的分区里读 `build.prop`** 得到的，再由
`adopt_installed_version()` 写给 keymint。退回基线后读到的是**更新前**的 `build.prop`：

- 若这次更新的补丁级别没变 → 恰好能工作；
- 若补丁级别变了（常见）→ keymint 被告知旧的补丁级别，而 `/data` 的密钥是在新补丁下
  封装的 → **仍然解不开**，但此时四个服务都显示"已注册"。

也就是说，B 把一个**明确的失败**（"有更新在飞，去系统里合并"）换成一个**看着像成功、
其实解不开**的状态。这比现状更糟，所以不该做。

真要做得安全，就必须拿到**新的** `build.prop`——而它在 COW 里，没有 snapuserd 读不到。
所以没有 A 就没有安全可言的 B。

### 结论

**A 是唯一真正的解。** 在拿到真机之前，正确的做法是保持现状：
整体拒绝、界面与 `skyward check` 明说是"有更新在飞、去系统里合并"。
这条现状本身是对的，不是缺口。

A 仍然值得做，但它必须**先有真机**：没有设备时写协议层等于赌。

## 六、验证方式

A 的验证（每一档都能独立判定成败）：

| 档 | 验证 |
| --- | --- |
| 能把 daemon 跑起来 | 在真机上手工执行系统的 `snapuserd`，确认它建出 `/dev/dm-user/<name>` 并接受连接（这一步不改任何数据） |
| 读得对 | 通过 dm-user 设备读一个快照分区，与"进系统后读到的内容"逐块比对 |
| 合并安全 | 先在**可丢弃**的机器/备份上验证合并；失败时能确认源分区未被改动 |
| 端到端 | `skyward check` 四服务注册、`/data` 解开、`an update in flight` 转为已合并 |

## 七、与"非 A/B 更新包"的关系

xuanyuan 是 A/B 机型，非 A/B 的包在它上面永远用不到。已有的安全行为是：
包若声明了别的机型就拒绝（`declares_other_device`，逗号与竖线两种分隔都认）。
除非目标改成"支持某台非 A/B 机型"，否则这一项没有可做的内容。
