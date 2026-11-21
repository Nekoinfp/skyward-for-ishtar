# Skyward for Xiaomi 13 Ultra (ishtar)

Skyward 是一个用 Rust 写的第三方 recovery。这是它针对 **小米 13 Ultra（ishtar）**
的适配版本。

- 上游：[kirklin/skyward](https://github.com/kirklin/skyward) 0.3.1
- 本版本：**0.3.1-beta1**
- 机型：Xiaomi 13 Ultra（`2304FPN6DC`）
- 底包：HyperOS `OS3.0.308.0.WMACNXM` 原厂 recovery

> ⚠️ **测试版，只在一台真机上验证过。** 刷入前请自己备份原厂 recovery。

---

## 它能做什么

和原厂 recovery 一样能刷机，但界面更完整、能解开加密的数据分区。

- **点亮、触摸、120 Hz**：界面是自绘的，中文排版、滑动都正常
- **解开 `/data`**：进「解密」页输入锁屏 PIN，内部存储和个人数据都能读
- **安装**：`adb sideload`、内部存储、U 盘三种来源，可一次选多个包依次装，
  刷 `.img` 时能只刷指定分区和槽位
- **备份 / 恢复 / 挂载 / 文件管理 / 终端 / 双系统**：都在「高级」和各页里
- **系统更新**：在线检查与下载

## 已知的限制

- **不要在 recovery 里装官方完整包**，大概率卡米。请在系统内更新。
- **压缩快照的合并**没开启；退路是重启进系统，系统会自己合并。
- **逻辑分区就地扩容**做不到。

---

## 安装

手机进入 fastboot，两个槽都刷（避免某次重启回落到旧的那一份）：

```bash
fastboot flash recovery_a skyward-v0.3.1-beta1-ishtar-OS3.0.308.0.WMACNXM.img
fastboot flash recovery_b skyward-v0.3.1-beta1-ishtar-OS3.0.308.0.WMACNXM.img
fastboot reboot recovery
```

进 recovery 后到「解密」页输入锁屏 PIN，`/data` 与内部存储就都能读。

### 底包

本项目在 **HyperOS 3.0.308.0.WMACNXM** 上验证通过。手机若换了系统版本，
用它自己那份 recovery 重打包更稳：

```bash
adb shell su -c 'dd if=/dev/block/bootdevice/by-name/recovery of=/sdcard/recovery.img'
adb pull /sdcard/recovery.img
```

### 刷了别的 recovery 起不来时

不是所有 recovery 镜像这台手机都认。刷了不认的，会直接掉回 fastboot，
而且槽位被标记成不可启动——**这时候再刷能用的镜像也会先掉 fastboot**。
先清标志再刷：

```bash
fastboot getvar slot-unbootable:a     # 看是不是 yes
fastboot set_active a                 # 清掉
fastboot flash recovery_a <可用的镜像>
```

---

## 从源码打包

```bash
# 底包：只取 recovery 分区，不下载整个 7.5 GB 包
target/release/skyward-pack stock --config devices/ishtar.toml --out stock/recovery.img

# 镜像
target/release/skyward-pack recovery \
    --stock stock/recovery.img \
    --binary target/aarch64-linux-android/release/skyward \
    --system-init target/system-init/aarch64-linux-android/release/system-init \
    --permissive-init target/system-init/aarch64-linux-android/release/permissive-init \
    --config devices/ishtar.toml \
    --font resources/fonts/MiSansVF.ttf \
    --mono resources/fonts/KirkMono-Regular.ttf \
    --mono-license resources/fonts/KirkMono-OFL.txt \
    --omapi resources/omapi \
    --out out/skyward-ishtar.img
```

## 许可

GPL-3.0-or-later，继承上游，详见 [LICENSE](LICENSE)。

上游：[kirklin/skyward](https://github.com/kirklin/skyward)。
界面字体 MiSans 与 Kirk Mono 均为 SIL OFL 1.1。
15 Ultra 的适配（本项目的代码基础）：[skyward-for-xuannyuan](https://github.com/Nekoinfp/skyward-for-xuannyuan)。
