# xuanyuan 适配验证报告

这份报告汇总的是**在本机（Linux/WSL，无手机）上能够确证的一切**，以及**真机上还需要验证什么**。
一句话结论：

> 解密链、逻辑分区扩容、sparse super 写入这三块的实现与前提都已核对到实物固件层面，
> 并有可复现的测试；但**镜像没有在任何手机上运行过**，压缩快照/增量 OTA 未实现。

凡是本节没有写"实测"的，都不是实测。

---

## 一、交付物

| 项 | 值 |
| --- | --- |
| 镜像 | `skyward-v1.1.0-xuanyuan-OS4.0.4.0.XOACNXM.img` |
| 大小 | 104,857,600 字节（与底包 recovery 分区同尺寸） |
| SHA-256 | `f2a160704717b317e1532f16c288ad50ee33d33b6057a4e2fd10ecb35e084a84` |
| 形状 | `ANDROID!`、header_version 4、kernel 0、ramdisk 51,256,277 字节 |
| 内含 recovery 本体 | 8994312 字节，AArch64 ELF |
| 底包 | OS4.0.4.0.XOACNXM，recovery 分区 SHA-256 `55f491052a360bc15362fd80c8bd2dd0912b289e1653f2bd7f9c5a12e7fea27b`（打包器自行核对） |

结构审计（`tools/audit-image.py`）32 项全过：boot 头、ramdisk 解包为 newc 并以
`TRAILER!!!` 结束、`system/bin/recovery` 为 AArch64 ELF、`system/bin/skyward` 指向
`recovery`、AVB footer 指向的 vbmeta 中哈希描述符的摘要**确实等于
`sha256(salt + 镜像)`**、属性描述符携带底包的 fingerprint。

同一份镜像重建两次哈希相同（构建可复现）。

---

## 二、可复现的验证

在 Linux（WSL 里的 Ubuntu 24.04，rustc 1.100.0，NDK r29）上执行 CI 自己的命令：

| 命令 | 结果 |
| --- | --- |
| `cargo test --workspace --locked` | 45 个 `test result` 块全绿，0 失败 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 退出码 0 |
| `cargo ndk -t arm64-v8a --platform 30 build --locked --release -p skyward` | 退出码 0 |
| 两个静态首程序（`RUSTFLAGS=-C target-feature=+crt-static`） | 退出码 0 |
| `cargo build --locked --release -p skyward-pack` | 退出码 0 |

其中与本适配直接相关的用例数：`skyward-install` 169、`skyward-lp` 23、
`skyward-block` 37、`skyward-pack` 13、`skyward` 二进制自己 22（报告 9、硬件描述改写 4、库拷贝 3、目录搜索 6），`skyward-sys` 47（含描述符移交与"进程还在跑吗"）、`skyward-vab` 39（含 dm-user 与 snapuserd 控制协议 9 项）。

稳定性：整个 workspace 连跑 15 次无失败；修掉一处偶发（并行测试把彼此的 `flock`
当成对手）后，avbctl 连跑 60 次无失败。

顺带修好了这份 fork **原本就存在**的三处失败（`copy()` 读出错时丢缓冲字节、
`spans()` 把正好三窗口当"太短"、`unzip -t` 从不报坏 CRC）——它们原本会让 CI 一直是红的。

---

## 三、对固件的实物核对

把 4.0.4.0 底包里的分区逐个取出、挂载、核对（全部在本机完成）：

| 分区 | 大小 | 核对到的结论 |
| --- | --- | --- |
| `recovery` | 104,857,600 | 哈希与配置一致；72 个 `system/lib64/*.so` |
| `vendor_dlkm` | 87,343,104 | 391 个模块；配置点名的 4 个都在；`modules.dep` 在 |
| `system_dlkm` | 15,282,176 | 有内容的是 `flatten/lib/modules`（96 个模块 + `modules.dep`），`lib/modules` 本身是空的 |
| `vendor` | 742,723,584 | `/vendor/etc/fstab.qcom` 的 `/data` 行含 `metadata_encryption=aes-256-xts:wrappedkey_v0`；配置启动的 9 个程序全在；`libhardware.so`、`lib64/hw` 在；`libvndksupport.so` **不在** |
| `odm` | 4,563,202,048 | weaver 服务在；`ese_weaver_thales.so`、`mi_weaver.so` 在 `/odm/lib64`；`libjc_keymint*.so` 属于 StrongBox 那个 keymint，不是本配置启动的 |
| `system` | 977,719,296 | 925 个库（recovery 只有 72）；`libvndksupport.so` 在 `system/lib64`，`libdl_android.so` 在 `system/lib64` 是链接进 runtime APEX、真副本在 `bootstrap/`；`system/bin/snapuserd` 与 `libsnapshot.so` 都在 |

三条关键结论：

1. **`/data` 是包裹式加密**：元数据密钥由 `wrappedkey_v0` 包着，解包要走 keymint，
   锁屏保护走 weaver。所以"密钥服务起不来 → /data 打不开"的因果成立。
2. **缺的库只有两个，位置还不同目录**：
   `keymint → libhardware.so(vendor) → libvndksupport.so(仅 system) →
   libdl_android.so(system/bootstrap)`。`provide_system_libraries()` 逐个库搜索
   `lib64` 与 `lib64/bootstrap`，两条都取得到；只查 `lib64` 会漏第二个。
3. **整张 fstab 解析无误**：62 行非注释条目 → 62 条解析结果，`/data` 两行
   （f2fs 与 mifs）字段全对。真机那两行已作为回归用例。

**自己复现这套核对**——一条命令（需要 cargo；`--firmware` 那步需要 root 或能 loop 挂载）：

```
skyward-pack stock --config devices/xuanyuan.toml --out stock/recovery.img --partition vendor
skyward-pack stock --config devices/xuanyuan.toml --out stock/recovery.img --partition odm
sudo mount -o loop,ro stock/vendor.img /mnt/vendor
sudo mount -o loop,ro stock/odm.img /mnt/odm

tools/verify.sh --config devices/xuanyuan.toml \
    --image out/skyward-v1.1.0-xuanyuan-OS4.0.4.0.XOACNXM.img \
    --firmware /mnt
```

（`--firmware` 指的那个目录下要有 `vendor/` 与 `odm/` 两个子目录。）

输出（实测）：

```
cargo test --workspace --locked                ok
cargo clippy --workspace --all-targets -- -D warnings ok
the stock recovery's SHA-256 is the one named  ok
the image audits (boot header, ramdisk, AVB digest) ok
every path the configuration names is in the firmware ok

5 passed, 0 failed, 0 skipped
```

脚本的取舍：分区没挂上时它**说"分区没挂上"并跳过**，而不是把每条路径都报成缺失；
任何一项失败都以非零退出，可以直接放在 CI 或发布脚本里。

`paths` 只核 `/vendor` 与 `/odm`：`/system` 下的路径是打包时塞进 ramdisk 的
（`se_omapi`、recovery 自己的 `lib64`），不在手机的这两个分区里，命令会把它们单列出来。
某个分区没给全时命令**直接失败**并说明该加哪个 `--`，不让"只查了一半"冒充通过。

### 真机实测（2026-11-19，首次）

一台已解锁**未**成功的小米 15 Ultra 经 USB 调试连上电脑，只读地取了以下数据。

**设备身份与状态**

| 项 | 值 |
| --- | --- |
| 机型 | `xuanyuan` / 25019PNF3C |
| 系统 | `OS4.0.4.0.XOACNXM`（与配置里的底包一致） |
| Android | **17（SDK 37）**；`ro.vendor.build.version.sdk` = **35** |
| 安全补丁 | `2026-10-01`（与从底包提取的值一致） |
| 当前槽 | `_a` |
| bootloader | **已锁**：`verifiedbootstate=green`、`device_state=locked`、`devicelock=1` |
| OEM 解锁开关 | **`oem_unlock_allowed=0`（关闭）** |
| 区域 | CN |

**与固件核对一致的项**

| 项 | 结果 |
| --- | --- |
| `/vendor/etc/fstab.qcom` 第 88 行 | 与我从底包提取的**逐字节相同**：`metadata_encryption=aes-256-xts:wrappedkey_v0`、`fileencryption=…wrappedkey_v0`、`keydirectory=/metadata/vold/metadata_encryption` |
| `/odm/etc/fstab.qcom` | 这台**没有**（代码按 NotFound 跳过） |
| `libvndksupport.so` | 51,504 字节，真机与镜像**同大小** |
| `/dev/dm-user/` | 存在（当前为空） |

**新得到、且改变判断的三条**

1. **`ro.virtual_ab.userspace.snapshots.enabled = true`**，`compression.enabled = true`。
   也就是说**压缩快照（VABC）是这台机器正常的 OTA 通道**。于是"未合并的压缩快照会让
   `mount_partitions()` 整体失败、`/data` 解不开"不是假设，是**真实可达的状态**。
2. **版本落差是真的**：系统 SDK 37 / Android 17，而 vendor 是 SDK 35、bootloader 的
   `swinfo.fingerprint` 写着 `xuanyuan:15/…`。这验证了打包器剔掉版本属性、
   由 recovery 从已安装系统 adopt 的必要性——差的是**两个大版本**，不是小补丁。
3. **一处纠错**：`/system/lib64/libdl_android.so` 在真机上是**指向 runtime APEX 的符号链接**，
   真实副本在 `bootstrap/`。我先前只看镜像时记成"只在 bootstrap"，是 `[ -e ]` 跟随悬空
   链接导致的误判。见第三节的更正。

**结论：这台机器刷了，而且 `/data` 解开了。** 起先 `ro.boot.verifiedbootstate=green`、
`ro.boot.flash.locked=1`、`oem_unlock_allowed=0` 读起来像未解锁，实际是装在机上的
Magisk 模块（`tricky_store` 等）把状态伪装了。`fastboot getvar unlocked` 给出
**`unlocked: yes`**——fastboot 直连 bootloader，模块改不了这个值。

**刷入与结果（2026-11-19）**

```
fastboot flash recovery_a skyward-v1.1.0-xuanyuan-OS4.0.4.0.XOACNXM.img   OKAY
fastboot flash recovery_b skyward-v1.1.0-xuanyuan-OS4.0.4.0.XOACNXM.img   OKAY
fastboot reboot recovery
```

第一次启动后 `skyward check` 报出**六个密钥服务全部 NOT registered**。查 dmesg 拿到根因：

```
servicemanager: getRecoveryHalManifest: VINTF parse error: Cannot add manifest fragment
/system/etc/vintf/manifest/weaver-thales.xml: HAL "android.hardware.weaver" has a conflict:
Conflicting FqInstance: IWeaver/default (@2) (from android.hardware.weaver-service.thales.xml)
vs. IWeaver/default (@1) (from weaver-thales.xml).
```

两份描述声明了同一个实例：ramdisk 里打包器放的 `weaver-thales.xml`（@1）与已安装系统提供的
`android.hardware.weaver-service.thales.xml`（@2）。**读者遇到冲突会丢掉整份清单**，
于是一个服务都找不到——gatekeeper 报 "Could not find … in the VINTF manifest"，
secure_element 连懒启动都失败。

修法是让**已安装系统的描述优先**：写入系统的描述时，删掉同目录下声明了同一实例的旧描述。
修好后重新刷入，`skyward check` 变成：

```
the services the key chain needs
  registered: android.hardware.security.keymint.IKeyMintDevice/default
  registered: android.hardware.gatekeeper.IGatekeeper/default
  registered: android.hardware.secure_element.ISecureElement/eSE1
  registered: android.se.omapi.ISecureElementService/default
  registered: android.hardware.weaver.IWeaver/default
  registered: android.hardware.boot.IBootControl/default
```

**`/data` 解开了：**

```
/dev/block/dm-9 on /data type f2fs (rw,...,inlinecrypt,...)
477G  366G 已用  111G 可用        /data
```

`/data/media/0`、`/data/system/users/0` 都是真实的用户数据内容。

这一条是本适配从第一天起要解决的事，现在**在真机上成立**。

---

## 四、已经查过、确认没问题的地方

这些是查之前怀疑有问题、查完发现已经做对了的，写下来免得以后重复调查：

| 怀疑 | 实际 |
| --- | --- |
| 手机上有未合并快照时，刷 super 或扩容会毁掉它 | 四处都会拒绝：`install_system`、`update_system`、刷 `ImageKind::Super`、`flash_logical`（`Refusal::Pending`）；脚本安装则在更新未完成时**不映射**该槽的分区 |
| 被拒绝时用户不知道该干什么 | 提示是"{快照状态}。请重启到系统，完成上一次更新后重试"，中英繁体都有 |
| 压缩快照更新失败时界面会让人以为在等 | 状态文案是"此类更新需在系统中合并，启动系统后将自动合并" |
| 配置里点名的库路径漏了 `/odm/lib64` | 查过二进制依赖：`/odm/lib64` 里那几个 `libjc_keymint*` 属于 **StrongBox** 版 keymint，配置启动的是 TEE 版（链接 `/vendor/lib64/libqtikeymint.so`），不需要加 |
| 两个 crate 各有一份"进程还在跑吗" | 确实重复。已合并为 `skyward_sys::running`，并补了真正起进程的测试 |
| 压缩快照读不了时，能不能退回"更新前的内容"先对付用 | **不能**：版本与补丁级别是从挂载分区的 `build.prop` 读的（`installed_props`），退回旧内容会把补丁读旧，keymint 仍会拒密钥，而四个服务却显示"已注册"——比现在的明确拒绝更糟。详见 `docs/vabc-design.md` |

---

## 五、尚未验证的部分

| 项 | 为什么 |
| --- | --- |
| ~~`/data` 在真机上能否解密~~ | **已验证（2026-11-19）**：六个密钥服务注册、`/data` 以 `dm-9` 挂上、477G/366G 真实数据可读 |
| OTA 安装、切槽、开机 | 需要设备，且要冒失败风险 |
| 逻辑分区扩容后能否挂载 | 需要设备（`super` 元数据改动不可逆，先备份） |
| sparse super 实际写盘 | 需要设备 |
| 界面与触摸、WLAN、振动、亮度 | 需要人看着屏幕 |
| 压缩快照/增量 OTA | 未实现。**但不是手机不支持**：内核已编入 `dm-user`，系统的 `snapuserd` 是静态二进制，两者都在。缺的是 recovery 侧的控制逻辑；**协议已查清并与本机 daemon 核对过**（12/13 控制消息匹配），详见 `docs/vabc-design.md` |
| 非 A/B 更新包 | 未实现：xuanyuan 是 A/B 机型，此类包用不上；只加了"包声明了别的机型就拒绝"的安全门 |

---

## 六、真机验证步骤

设备要求：已解锁的小米 15 Ultra，系统 OS4.0.4.0。

**先备份**（不可跳过）：

```
fastboot fetch recovery_a stock-recovery_a.img     # 需要较新的 fastboot
fastboot fetch recovery_b stock-recovery_b.img
```

**刷入**（两个槽位都刷，避免回落到旧的那一份）：

```
fastboot flash recovery_a skyward-v1.1.0-xuanyuan-OS4.0.4.0.XOACNXM.img
fastboot flash recovery_b skyward-v1.1.0-xuanyuan-OS4.0.4.0.XOACNXM.img
fastboot reboot recovery
```

**观察点与期望**：

1. **界面出现、触摸可用**。若黑屏，说明 recovery 服务根本没起来——这本身是重要信息，
   请抓 `adb shell dmesg | grep -i skyward`。
2. `adb shell skyward check` 的输出。期望：
   - `the services the key chain needs` 下 keymint / gatekeeper / secure_element / weaver
     都是 `registered`；
   - `the key it keeps:` 后面**不是** `empty`；
   - 表里 `metadata_encryption:` 不是 `(none)`。
3. 首页/设置页里存储容量能显示、`/data` 不再显示"未解密"。
4. `adb shell dmesg | grep -i skyward` 里不应出现
   `devices not brought up` 或 `partitions not mounted`。

**如果 `/data` 解不开，先看这两处再怀疑镜像**：

- `skyward check` 里 `an update in flight:` 是不是 `Snapshotted` 而不是 `None`。
  若手机上有**未合并的压缩快照**，`mount_partitions()` 会**整体失败**——快照无法由
  snapuserd 服务时 `map_snapshot()` 直接报错，而这个错中止整个挂载循环，于是 `vendor`
  没挂上、keymint 起不来、`/data` 解不开。**表现与"镜像坏了"完全一样。**
  处理办法：正常启动一次系统让它合并，再回 recovery 试。
- `skyward check` 里 `the services the key chain needs` 下有没有 `NOT registered`。
  有的话，`/skyward/run/services/<名字>.log` 里就是它退出的原因（缺哪个库、哪个节点）。

**要回报的东西**（一条命令打包）：

```
tools/collect-report.sh report.txt
```

它收 `skyward check`、`dmesg`、`recovery.log`、每个服务的 stderr、mounts、两份 fstab、
属性。若 `adb` 不可用，至少把 `skyward check` 的输出和
`/skyward/run/services/*.log` 发回。

**风险提示**：扩容前请 `dd if=/dev/block/by-name/super of=/sdcard/super_stock.img`
备份；提交元数据不是掉电原子操作。sparse/大镜像写完只 sync 与窗口比对，**刷完请立即重启**。

---

## 七、这份报告的边界

- 所有"实测"都指**在本机对固件文件与代码的核对**，不含真机运行。
- 「配置里某个路径存在」证明的是**文件在**，不证明 HAL 能成功初始化——
  后者只能真机验证。
- 镜像哈希只对应本机这次构建；从 CI 打出会不同（时间戳等），内容应一致。
