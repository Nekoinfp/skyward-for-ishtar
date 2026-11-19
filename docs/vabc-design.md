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

**协议已经查清**（AOSP `platform/system/core`，`fs_mgr/libsnapshot/snapuserd/`）：

**1. 内核 ↔ 用户态（dm-user 设备）**

- daemon 打开 `/dev/dm-user/<misc_name>`（`O_RDWR`），内核用这个字符设备收发请求。
- 请求头是 40 字节的 `struct dm_user_header { u64 seq, type, flags, sector, len }`（packed）。
  `type`：`DM_USER_REQ_MAP_READ = 0`、`DM_USER_REQ_MAP_WRITE = 1`。
  **写请求不会被接受**——分区是只读挂载的。
- 回复：把同一个头改写为 `DM_USER_RESP_SUCCESS = 0`（或 `DM_USER_RESP_ERROR = 1`），
  与数据一起写回；后续分片只写数据、不再带头。
- COW 盘上格式：`disk_header { magic = 0x70416e53, valid, version = 1, chunk_size }`，
  其后是 `disk_exception { old_chunk, new_chunk }` 表。

**2. 控制方 ↔ daemon（文本 socket）**

抽象命名空间（`ANDROID_SOCKET_NAMESPACE_RESERVED`）的 `SOCK_STREAM`，消息是逗号分隔的纯文本：

| 发送 | 回复 |
| --- | --- |
| `query` | `active` / `passive` |
| `init,<misc>,<cow>,<backing>[,<base_path_merge>]` | `success,<扇区数>` |
| `start,<misc>` | `success` |
| `delete,<control device>` | `success` |
| `initiate_merge,<misc>` / `pause_merge` / `resume_merge` | `success` |
| `merge_percent` | 数字 |
| `getstatus,<misc>` | 状态串 |
| `update-verify` | `success` |
| `supports,second_stage_socket_handoff` | `success` |
| `stop` / `detach` | — |

daemon 由 init 启动（`ctl.start=snapuserd`），就绪属性 `snapuserd.ready`，
存活标记文件在 `/metadata/ota/` 下。

**3. 这台手机上的 daemon 与 AOSP 的协议对得上**

拿 4.0.4.0 的 `system/bin/snapuserd`（静态、AArch64）逐条比对 AOSP main 的客户端代码：

| 消息 | 在该二进制里 |
| --- | --- |
| `query`、`start`、`init`、`delete` | 各 1 处精确匹配 |
| `initiate_merge`、`merge_percent`、`getstatus`、`update-verify` | 各 1 处 |
| `pause_merge`、`resume_merge`、`detach`、`stop` | 各 1 处 |
| `second_stage_socket_handoff` | **没有** |
| `/dev/dm-user/`、`snapuserd.ready`、`snapshot-merge` | 都在 |

也就是说：**核心控制协议与 AOSP 一致，但这台的 daemon 早于"两阶段 socket 移交"那版**
（`second_stage_socket_handoff` 是更新才有的能力）。实现时要走**旧的 attach 路径**，
不能假设有移交。

（`init.svc.snapuserd` 在这个二进制里没有属正常：那是 libsnapshot **客户端**用的字符串，
不在 daemon 里；daemon 自己设的是 `snapuserd.ready`，它有两处。）

**4. 协议代码已经写好并测过**（`crates/skyward-vab/src/dmuser.rs`，9 项测试）：

- `Header`：40 字节的编解码、应答构造、长度是否装得下；
- `Asked` / `Answered`：**同一个数值在两个方向上含义不同**（0 = 读 / 成功，1 = 写 / 错误），
  所以用两个枚举按方向解读，而不是一个整数加注释；
- `Request` / `Reply`：13 条控制消息与回复的文本编解码，未知消息**报错而非静默丢弃**。

**5. 内核侧已经在真机上打通**（`skyward dm-user <名字>`）

三条都是**实测**出来的，不是从文档抄的：

| 事实 | 怎么知道的 |
| --- | --- |
| target 名是 **`user`**，不是 `dm-user` | 问 `dm-user` 内核说没有；问 `user` 答 **1.0.0** |
| 参数**必须是 3 个** | 给 2 个，内核日志写 `user: Invalid argument count (-EINVAL)` |
| 名字取自**第 3 个**参数 | `0 1 name` 与 `1 0 name` 都建出 `/dev/dm-user/name`；`name 1 0` 建出的是 `/dev/dm-user/0` |

所以现在写的是 `0 0 <名字>`（前两个内核根本不读，填 0 并注明）。实测输出：

```
the kernel's target is "user" 1.0.0
made /dev/block/dm-10 (8 sectors)
/dev/dm-user/skywardprobe2: there
took skywardprobe2 away again
```

**6. 守护进程也已握手成功**（`skyward snapuserd`）

- 从已安装系统启动 `-user_snapshot`，它**会留在前台**；
- 它监听的不是抽象命名空间，而是**文件 socket `/dev/socket/snapuserd`**
  （从它自己的 fd 表看出来：`socket:[...] → /dev/socket/snapuserd`）；
- 发 `query` 得到 **`Active`**——协议通了；
- 对 `init,…,/dev/null,/dev/null`（不是快照）它答 `fail`，是正确拒绝。

实测输出：

```
the daemon at /mnt/system/system/bin/snapuserd: not running
  query: Active
the daemon is active and would serve blocks
  it answered "fail", which is its own word for refusing a device that is not a snapshot
```

**7. 已经接通，并在真机上验证**（2026-11-19）

手机在**系统里**做了一次正常更新（只有这条路会产生用户态快照；recovery 的
`update_engine_sideload` 会关掉 VABC）。结果：

```
/metadata/ota/snapuserd_mode = dm-user
9 个分区：system_b product_b odm_b vendor_b vendor_dlkm_b
          system_dlkm_b system_ext_b mi_ext_b mi_product_b
system_b 的 proto：using_snapuserd=1，compression_algorithm="lz4"
```

在 B 槽的 recovery 里，Skyward 把**九个分区全部挂上**：

```
skyward: mounted odm_b on /odm (erofs)
skyward: mounted product_b on /mnt/product (erofs)
skyward: mounted system_b on /mnt/system (erofs)
… 共 9 个
```

从快照里读出的文件是真的（`snapuserd`、`qseecomd`、`build.prop`），
**六个密钥服务全部注册**。设备映射的完整形状：

- 9 × `{分区}-base`（super extents 的 dm-linear）
- 9 × `{分区}-cow-img`（`/metadata/gsi/ota/lp_metadata` 描述的 COW 镜像）
- `odm_b-cow`（super 里的 COW 分区）
- 9 × `user_snapshot-{分区}` 的 dm-user 设备与 `/dev/dm-user/` 控制设备

三件事是从**手机自己的抱怨**里学到的：

1. **`init` 要 5 段**，不是 4 段。设备日志：`Malformed init message, 4 parts`。
   第 5 段是它能重新打开 base 设备的路径。
2. **dm-user 设备要先建**。这版 daemon 在 `init` 时就挂 worker，
   日志：`Could not open dm-user path: /dev/dm-user/…: No such file or directory`。
   （AOSP 的顺序是先 init 再建设备，这版不同。）
3. **daemon 必须在 ramdisk 里**。它服务的正是 system 分区，从已安装系统取是鸡生蛋。
   打包器为此加了 `--snapuserd`。

**合并（merge）试过，但不开启**（2026-11-20）

对系统自己写出的未合并快照试了两次：

| 尝试 | 结果 |
| --- | --- |
| 第一次 | `product_a` 合并完成；`system_a` 开始后**卡住** |
| 第二次（把 dm-user 设备改成可写） | 一个都没完成，同样在 22% 处卡住 |

卡住的证据：daemon 的 CPU 时间**十秒无变化**、无磁盘写入、`/tmp/merge.log` 一直停在 `Going(0.222…)`。

**挂住比拒绝更糟**，所以 `initiate`/`check` 对压缩快照重新明确拒绝，消息里直接说"系统会自己在启动时合并"。

实测这条退路可用：从这个半合并状态重启，**系统用约一分钟把九个分区合并完**，`snapuserd` 停止，`/data` 从 404G 降到 394G（COW 空间释放）。

未查清的原因（留给以后）：daemon 在 `init` 时收到的第五个参数是"重新打开 base 设备的路径"，给的是 dm 设备路径；AOSP 那边更像是一个**目录**。这可能就是它卡住的地方。
### 合并为什么卡住——查到的确切协议

从 AOSP 的 `snapuserd_server.cpp`（`user-space-merge/`）读到：

```cpp
// init,<misc_name>,<cow_device_path>,<backing_device>,<base_path_merge>
if (out.size() != 5) { LOG(ERROR) << "Malformed init message, " ...; return Sendmsg(fd, "fail"); }
auto handler = AddHandler(out[1], out[2], out[3], out[4], std::nullopt);
```

而客户端 `snapuserd_client.cpp` 的 `InitDmUserCow(misc, cow, backing, base_path_merge)` 是：

```cpp
if (base_path_merge.empty()) parts = {"init", misc_name, cow_device, backing_device};        // 4 段
else                          parts = {"init", misc_name, cow_device, backing_device, base_path_merge};  // 5 段
```

**`base_path_merge` 不是可选的**：daemon 只接受 5 段。它的用途是让 daemon 在合并时**按路径重新打开 base 设备**
（读的时候可以用 init 时拿到的 fd，写回时要另开一个可写的）。

我给的第 5 段就是 base 的 dm 设备路径（`/dev/block/dm-N`）。这可能不是它要的形式——
AOSP 那边这个参数叫 `base_path`，更像**目录**或分区路径。这是下一步最该试的地方。

另外两处协议细节，也一并记下：

- `merge_percent` **不带参数**，返回一个全局百分比（`handlers_->GetMergePercentage()`）；
  22.2% = 2/9 正好对应"两个 handler 有合并状态"。所以卡住时这个数不动是准确的信号。
- `initiate_merge,<misc>` 回 `success` 才算开始；`getstatus,<misc>` 回 `snapshot-merge-complete` /
  `snapshot-merge-failed`。

**第三次实测（2026-11-20，逐分区状态）**推翻了我自己前面的一个错误判断：我以为合并"卡住"了，其实是**我只问了全局百分比**。用 `getstatus,user_snapshot-<分区>` 逐分区问之后：

| 分区 | 结果 |
| --- | --- |
| `odm_b`（COW 在 super 里） | **`snapshot-merge-complete`** |
| 其余八个（COW 在 `/metadata/gsi/ota/lp_metadata` 镜像里） | 停在 `snapshot-merge`，二十分钟不动，daemon 空闲 |

两个要点：

- `getstatus` 必须用 **daemon 认的名字** `user_snapshot-<分区>`。用分区名问，服务器找不到 handler 就回 `snapshot-merge-failed`——**那是名字找不到，不是合并失败**。我一开始就被这个骗了。
- 能完成的那个，正好是**唯一一个 COW 在 super 分区里**的。卡住的八个，COW 都在 scratch super 的镜像里。

所以下一步该试的是第 5 段 `base_path_merge` 换个形式（它管的就是 base 设备按路径重开），而**不是**继续折腾全局百分比。

结论不变：**没有可测的快照就不开这条路**。要验证，需要再一次更新产生未合并快照，
然后先试"第 5 段传别的形式"。