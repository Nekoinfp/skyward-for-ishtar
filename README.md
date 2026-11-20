# Skyward for Xiaomi 15 Ultra (xuanyuan)

[Skyward](https://github.com/kirklin/skyward) 是一个用 Rust 写的第三方 recovery。
这个仓库是它针对 **小米 15 Ultra（`xuanyuan`）**、**澎湃 OS 4.0.4.0（`OS4.0.4.0.XOACNXM`）**
的适配版本。

- 上游：[kirklin/skyward](https://github.com/kirklin/skyward) 0.3.1
- 本版本：**0.3.1-beta1**
- 目标机型：Xiaomi 15 Ultra（`25019PNF3C` / `25010PN30C`）
- 目标系统：OS4.0.4.0.XOACNXM（Android 17 / SDK 37 + vendor SDK 35）

> ⚠️ **测试版**。已经在一台真机上逐项验证过下面列出的功能，但它是 beta，
> 刷入前请自己备份能回滚的原厂 recovery。

---

## 这个适配做了什么

Skyward 的构造方式是**在手机自己的原厂 recovery 上重建**：解包官方包里的
`recovery` 分区（原厂 ramdisk 提供机型专属的驱动模块、服务、fstab），把
`system/bin/recovery` 换成 Skyward 本体，再由 `argv[0]` 决定跑 recovery 还是子命令，
`system/bin/skyward` 是指向它的软链接。

所以它**不需要另做一份内核**，也就不会因为内核版本不对而开不了机；换机型只需换配置
（`devices/xuanyuan.toml`）和底包。

针对这台手机做的适配：

| 项 | 说明 |
| --- | --- |
| `/data` 解密 | metadata 加密（`wrappedkey_v0`）+ keymint / gatekeeper / weaver / eSE 全链路 |
| 压缩快照（VABC） | 把手机自己的 `snapuserd` 放进 ramdisk，读 lz4 压缩快照挂载分区 |
| 屏幕与触摸 | 1440×3200@120、双击唤醒、触摸控制器唤醒时序 |
| 机型参数 | 处理器名、内存、电池、分辨率等从真机读 |

---

## 已验证的功能

在真实设备上逐项跑过：

- **`/data` 完整解密**：输入锁屏 PIN 后，设备加密（DE）与用户加密（CE）都打开——
  `/data/data` 可读、`/sdcard` 可读。**这是本适配修好的核心问题**。
- **压缩快照读取**：系统更新后产生的 9 个 lz4 用户态快照，全部挂载成功，
  六个密钥服务从快照里的分区正常注册。
- **界面、屏幕、触摸、WLAN**：都可用。
- **解密后查看文件、备份/恢复、终端、挂载管理**等页面功能。
- **安装**：`adb sideload`、内部存储、U 盘三种来源；可一次选多个 zip/img 依次安装；
  刷单个 `.img` 时可选择只刷指定分区（boot / init_boot / vendor_boot / recovery /
  dtbo / vbmeta）与槽位。
- **逻辑分区扩容规划**：`skyward lp --plan-grow` 只读地算一遍能不能扩（见下面"已知限制"）。

## 已知限制

- **不要在 recovery 里装官方完整包**。在 Skyward（或任何第三方 recovery）里刷官方
  完整 OTA，装完后大概率**卡米**：安装本身是成功的，但更新包会把 boot 与 vbmeta 刷成
  原厂，而这一代小米的 bootloader 有自己的签名校验，`disable-verification` 标志绕不过去。
  装完两个槽都会变成"不可启动"，连 recovery 都进不去，只剩 fastboot。
  **请始终在系统里更新**（设置 → 系统更新）。安装页与系统更新页都已用红字标明这条。
- **压缩快照的合并**没有开启。`skyward merge --compressed` 会逐分区驱动手机的
  snapuserd 合并，但实测 9 个分区里只有 1 个（COW 在 super 里那个）能完成，其余停在
  `snapshot-merge`。**退路是可靠的**：重启进系统，系统会在一分钟内自己合并完。
- **逻辑分区就地扩容做不到**。这台手机的 super 元数据里没有对齐的空闲区（唯一空洞
  1 MB），要扩必须搬 extent，而 `grow` 按设计不移动别人的扇区。OTA 不受影响。
- 增量更新包与非 A/B 更新包未实现（前者等上游发布新版本可测，后者这台机器用不上）。

---

## 安装

### 前提

- 手机**已解锁 bootloader**
- 系统版本 **OS4.0.4.0.XOACNXM**（底包必须一致，本镜像按这个版本的原厂 recovery 重建）
- 电脑装了 `adb` 与 `fastboot`

### 一、备份能回滚的原厂 recovery（不要跳过）

```bash
fastboot fetch recovery_a stock-recovery_a.img
fastboot fetch recovery_b stock-recovery_b.img
```

`fetch` 需要较新的 fastboot；不支持的话，先刷回官方完整包里的 `recovery` 分区即可。

### 二、刷入

两个槽都刷，避免某次重启回落到旧的那一份：

```bash
fastboot flash recovery_a skyward-v0.3.1-beta1-xuanyuan-OS4.0.4.0.XOACNXM.img
fastboot flash recovery_b skyward-v0.3.1-beta1-xuanyuan-OS4.0.4.0.XOACNXM.img
fastboot reboot recovery
```

### 三、第一次启动

进 recovery 后屏幕会亮起 Skyward 主页。

想看 `/data` 里的内容，进「解密」页输入**你平时的锁屏 PIN/密码**。
输入正确后 `/data` 与用户数据都会打开，`/sdcard` 也能读。

> 连续输错会被手机的 secure world 记次数（和锁屏上输错一样），注意别乱试。

### 校验（可选）

```bash
sha256sum skyward-v0.3.1-beta1-xuanyuan-OS4.0.4.0.XOACNXM.img
# 84bf36e7d2431034035948faf5d0243f9ce150d596a2d3dbd7f670f8ae22000c
```

---

## 在 recovery 里能做什么

- **解密**：打开 `/data`，之后文件页能看到内部存储
- **安装**：`adb sideload` / 内部存储 / U 盘；支持 A/B 完整包、带脚本的刷机包、
  分区镜像；可多选依次安装，可指定只刷某个分区
- **备份 / 恢复**：把分区备份成文件放到内部存储、U 盘或电脑
- **文件**：浏览、复制、移动、重命名、编辑文本、改权限
- **挂载**：管理 super 里的逻辑分区映射与挂载
- **终端**：内置 shell（Kirk Mono 等宽字体）
- **双系统**：把第二系统装进 data 分区并切换
- **系统更新**：在线检查与下载（页面上有关于官方完整包的红字警告）
- **清除数据 / 格式化分区**
- **AVB**：按需关闭某个槽的启动校验

命令行的能力更多（诊断用），`adb shell skyward --help` 可看全部子命令。常用的：

```bash
skyward check          # 说清楚 /data 打不开缺什么，不写任何东西
skyward slot           # 槽位与 boot control 状态
skyward lp             # super 里的逻辑分区
skyward data           # 挂载 /data 并打开各个用户自己的文件
skyward capture /tmp/x # 把当前画面存成原始 RGBA（无屏幕调试用）
```

---

## 从源码构建

需要 Rust 与 Android NDK（本适配用 NDK r29）：

```bash
# recovery 本体
cargo ndk -t arm64-v8a --platform 30 build --release -p skyward

# 两个先启动的静态程序
RUSTFLAGS="-C target-feature=+crt-static" cargo ndk -t arm64-v8a --platform 30 build --release \
    -p skyward-system-init -p skyward-permissive-init --target-dir target/system-init

# 打包进原厂 recovery
cargo build --release -p skyward-pack
target/release/skyward-pack recovery \
    --stock <官方包解出的 recovery.img> \
    --binary target/aarch64-linux-android/release/skyward \
    --system-init target/system-init/aarch64-linux-android/release/system-init \
    --permissive-init target/system-init/aarch64-linux-android/release/permissive-init \
    --config devices/xuanyuan.toml \
    --font resources/fonts/MiSansVF.ttf \
    --mono resources/fonts/KirkMono-Regular.ttf \
    --mono-license resources/fonts/KirkMono-OFL.txt \
    --omapi resources/omapi \
    --snapuserd <官方包里的 system/bin/snapuserd> \
    --out out/skyward.img
```

`--snapuserd` 是必须的：压缩快照要读的正是 system 分区，从已安装系统里取就来不及了。

跑测试与检查：

```bash
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

---

## 许可

继承上游 Skyward：**GPL-3.0-or-later**。详见 [LICENSE](LICENSE)。

界面字体 MiSans（小米，SIL OFL 1.1），终端等宽字体 Kirk Mono（SIL OFL 1.1），
许可证原文在 `resources/fonts/`。

## 致谢

- [kirklin/skyward](https://github.com/kirklin/skyward) —— 上游，绝大多数设计与代码来自这里
- 底层库：`ab_glyph`（字体光栅化）、`prost`（protobuf）、`zerocopy` 等
