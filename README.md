# Skyward for Xiaomi 13 Ultra (ishtar)

Skyward 是一个用 Rust 写的第三方 recovery。

本项目是 **小米 13 Ultra（ishtar）、澎湃 OS 4.0.29（`OS4.0.29.0.XEOCNXM`）** 的适配版本。

- 上游：[kirklin/skyward](https://github.com/kirklin/skyward) 0.3.1
- 本版本：**0.3.1-beta1**
- 目标机型：Xiaomi 13 Ultra（`2304FPN6DC`）
- 测试系统：OS4.0.29.0.XEOCNXM（Android 17 / SDK 37）
- 底包：OS3.0.308.0.WMACNXM 原厂 recovery
  （`ed63e2d3cbb1b06a15579f5384c87e7d80505e44140b3a767f5034d532a3568a`）

> ⚠️ **测试版**。已经在一台真机上点亮、触摸、120 Hz 与全套密钥服务，
> 但**用户数据（CE）还没有解开过**。刷入前请自己备份原厂 recovery。

---

## 这台手机和 15 Ultra 不一样的地方

Skyward 的构造方式是在手机自己的原厂 recovery 上重建：内核、`init`、`adbd`、
SELinux 策略、库都是原厂的，只把 `system/bin/recovery` 换成 Skyward 本体。
所以换机型只需要换描述，但这台手机的描述与 15 Ultra 有四处实质差异：

| | 13 Ultra（ishtar） | 15 Ultra（xuanyuan） |
| --- | --- | --- |
| 触摸 | ST FTS over SPI（`fts_touch_spi`），走 `touch_report` | Synaptics TouchComm |
| 屏幕 | 1440×3200，密度 560，120 Hz | 1440×3200，密度 600 |
| 密钥服务 | **HIDL**：gatekeeper/boot/安全元件都走 `hwbinder` | 全 AIDL |
| weaver | **没有独立进程**，由 keymint 的 AIDL 实现回答 | odm 上的 thales |
| snapuserd | 原厂 recovery **自带** | 原厂没有，打包时补进去 |

HIDL 这一条是适配的主要难点：recovery 里没有 `hwservicemanager`，
而 HIDL 服务没有它就没有注册中心。Android 从 `system_ext` 跑它，
本适配把它作为手机自己的一个程序启动。

## 已验证的功能

在真机上跑过：

- **点亮与界面**：主页、设置页、中文排版全部正常
- **触摸**：可用
- **120 Hz**：面板的 `1440x3200x120cmd@120x60auto` 模式，`fastest_refresh = true`
- **九分区挂载**：system / system_ext / vendor / odm / product / mi_ext 等
- **密钥链全部就绪**：hwservicemanager、qseecomd、ssgtzd、keymint、
  gatekeeper、安全元件六个进程在跑
  - `skyward gatekeeper` 实测应答：*"the secure world does not recognise
    that credential"*——安全世界在工作
  - `skyward keymint` 应答 HAL 4 version 4
- **`/data` 设备加密（DE）解开**：`/data` 挂载为 `dm-9`，62 个顶层条目，
  `/data/data` 592 个应用目录可读
- **浏览器**：入口在「双系统」下方，能读网页、显示图片

## 还没验证的

- **用户加密（CE）**：`/sdcard` 与各用户自己的文件仍锁着，需要锁屏 PIN。
  输入后能否解开尚未确认——密钥链已就绪，但没试过真的凭据。
- **WLAN**：`qca_cld3_kiwi_v2` 走 `cnss2`，未验证
- **压缩快照读取**：原厂 recovery 自带 snapuserd，未验证

## 已知限制

- **不要在 recovery 里装官方完整包**，大概率卡米。请在系统内更新。
- **压缩快照的合并**没开启；退路是重启进系统，系统会自己合并。
- **逻辑分区就地扩容**做不到（super 里没有对齐的空闲区）。

---

## 安装

手机进入 fastboot 刷入镜像即可。两个槽都刷，避免某次重启回落到旧的那一份：

```bash
fastboot flash recovery_a skyward-v0.3.1-beta1-ishtar-OS3.0.308.0.WMACNXM.img
fastboot flash recovery_b skyward-v0.3.1-beta1-ishtar-OS3.0.308.0.WMACNXM.img
fastboot reboot recovery
```

进 recovery 后到「解密」页输入锁屏 PIN，`/data` 与内部存储就都能读。

### 底包必须对上

底包是 **OS3.0.308.0.WMACNXM**（这台手机的 vendor 分区就是它）。
手机如果跑的是别的版本，应当用它自己那一份 recovery 重打包：

```bash
adb shell su -c 'dd if=/dev/block/bootdevice/by-name/recovery of=/sdcard/recovery.img'
adb pull /sdcard/recovery.img
```

---

## 从源码打包

```bash
# 底包：只取 recovery 分区，按需读，不下载整个 7.5 GB 包
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

不需要 `--snapuserd`：这台手机的原厂 recovery 里本来就有，打包器会拒绝重复添加。

---

## 许可与致谢

继承上游 Skyward：**GPL-3.0-or-later**，详见 [LICENSE](LICENSE)。

上游仓库：[kirklin/skyward](https://github.com/kirklin/skyward) —— 绝大多数设计与代码来自这里。

界面字体 MiSans（小米，SIL OFL 1.1），终端等宽字体 Kirk Mono（SIL OFL 1.1）。

15 Ultra 的适配（本项目的代码基础）：[skyward-for-xuannyuan](https://github.com/Nekoinfp/skyward-for-xuannyuan)。
