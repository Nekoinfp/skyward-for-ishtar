# Skyward for the Xiaomi 13 Ultra (ishtar)

A Skyward recovery built on the 13 Ultra's own stock recovery, from the same
code as the 15 Ultra's adaptation. Everything that adaptation fixed is
generic — the key renewal that opens `/data`, the markup reader behind the
browser, installing several files at once — and the phone's own description is
upstream's `devices/ishtar.toml`, taken unchanged.

```
底包：OS3.0.308.0.WMACNXM 原厂 recovery
      recovery 分区 SHA-256 ed63e2d3cbb1b06a15579f5384c87e7d80505e44140b3a767f5034d532a3568a
镜像：69569d155c124c9334e6581f5fe846376c5c7feb6c8a41e5773619cab50a0774
大小：104,857,600 字节（整个 recovery 分区）
```

> ⚠️ **没有在真机上验证过。** 这份镜像是打包出来的，结构审计 32 项全过，
> 但还没有一台 13 Ultra 跑过它。刷入前请自己备份原厂 recovery。

## 它是什么

和 15 Ultra 那份一样：Skyward 的构造方式是**在手机自己的原厂 recovery 上
重建**，内核、`init`、`adbd`、SELinux 策略、库都是原厂的，只把
`system/bin/recovery` 换成 Skyward 本体。所以不需要另做内核，也不会因为内核
版本不对而开不了机。

13 Ultra 与 15 Ultra 的差异都在配置里，不在代码里：

| | 13 Ultra（ishtar） | 15 Ultra（xuanyuan） |
| --- | --- | --- |
| 触摸 | ST FTS over SPI（`fts_touch_spi`） | Synaptics TouchComm（`synaptics_tcm2`） |
| 屏幕 | 1440×3200，密度 560 | 1440×3200，密度 600 |
| snapuserd | **原厂 recovery 自带** | 原厂没有，打包时补进去 |
| 触摸节点移交 | 40 个节点交给 `system` 用户（原厂 init 脚本如此） | 不移交 |

## 刷入

先备份能回滚的原厂 recovery：

```bash
fastboot fetch recovery_a stock-recovery_a.img
fastboot fetch recovery_b stock-recovery_b.img
```

两个槽都刷：

```bash
fastboot flash recovery_a skyward-v0.3.1-beta1-ishtar-OS3.0.308.0.WMACNXM.img
fastboot flash recovery_b skyward-v0.3.1-beta1-ishtar-OS3.0.308.0.WMACNXM.img
fastboot reboot recovery
```

进 recovery 后到「解密」页输入锁屏 PIN，`/data` 与内部存储就都能读。

## 底包版本必须对上

这份镜像的底包是 **OS3.0.308.0.WMACNXM**。手机如果跑的是别的版本，应当用
**它自己那一份** recovery 重打包，而不是刷这份。

原因：recovery 的 AVB footer 把原厂的安全补丁级别交给引导程序，再传给安全世界；
与已装系统对不上时，KeyMint 会拒绝系统做出的密钥，`/data` 就打不开。

> 打包器有一次实测提示：`recovery.img names no security patch in its footer, as
> this phone's stock recoveries do not either` —— 13 Ultra 的原厂 recovery footer
> 里没有安全补丁级别，这个风险可能比 15 Ultra 小。但这只是打包器读到的，没有在
> 真机上验证过。

**最稳的底包是从手机自己身上取的**（晚上连上电脑就能做）：

```bash
adb shell su -c 'dd if=/dev/block/bootdevice/by-name/recovery of=/sdcard/recovery.img'
adb pull /sdcard/recovery.img
```

然后：

```bash
skyward-pack recovery --stock recovery.img --config devices/ishtar.toml ... 
```

## 已知的限制

与 15 Ultra 那份相同：

- **不要在 recovery 里装官方完整包**，大概率卡米（装完 boot 与 vbmeta 变原厂，
  这一代小米 bootloader 有自己的签名校验绕不过）。请在系统内更新。
- **压缩快照的合并**没开启；退路是重启进系统，系统会自己合并。
- **逻辑分区就地扩容**做不到。

还没做过的验证（真机连上后逐项补）：

- `/data` 能否用锁屏 PIN 解开
- 触摸与屏幕是否正常（ST FTS 走的是 `touch_report`，与 15 Ultra 的路径不同）
- WLAN 能否起来（WCN7850 走 `qca_cld3_kiwi_v2`）
- 压缩快照能否读取

## 从源码打包

```bash
# 底包：只取 recovery 分区，按需读，不下载整个包
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

不需要 `--snapuserd`：13 Ultra 的原厂 recovery 里本来就有，打包器会拒绝重复添加。
