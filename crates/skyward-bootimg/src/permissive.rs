//! A permissive kernel, in three places at once, each changed in place so
//! the rest of each partition (its AVB footer included) is left as it was:
//!
//! - `boot`: the kernel's own `enforcing=0` in the header's command line,
//!   so SELinux starts permissive before any policy is loaded;
//! - `vendor_boot`: `androidboot.selinux=permissive` in its bootconfig,
//!   for an init built to honour it (userdebug and eng builds);
//! - `init_boot`: a user build's init ignores both and switches SELinux to
//!   enforcing right after it loads the policy, so the ramdisk's first
//!   program becomes Skyward's [`PROGRAM`], which starts a watcher and then
//!   the ramdisk's own first program, kept as [`OWN_INIT`]. The watcher,
//!   still in the kernel's domain, turns enforcing off again as soon as init
//!   turns it on.
//!
//! Each change undoes cleanly: the images are the phone's own again after
//! [`boot`], [`vendor_boot`] and [`init_boot`] with `on` false.

use crate::image::BootImage;
use crate::ramdisk::{lz4_legacy, Ramdisk};
use anyhow::{bail, ensure, Context, Result};

/// The kernel's own switch, in the boot image's command line.
pub const KERNEL_ARG: &str = "enforcing=0";
/// Android's switch, in the vendor_boot image's bootconfig.
pub const BOOTCONFIG: &str = "androidboot.selinux=permissive";
const BOOTCONFIG_KEY: &str = "androidboot.selinux";
/// Where the ramdisk's own first program is kept.
pub const OWN_INIT: &str = "init.skyward";
/// The ramdisk's first program.
const INIT: &str = "init";

const BOOT_MAGIC: &[u8] = b"ANDROID!";
/// Where a version 3 or 4 boot header keeps its version and command line.
const BOOT_VERSION_AT: usize = 40;
const BOOT_CMDLINE_AT: usize = 44;
const BOOT_CMDLINE_LEN: usize = 1536;

const VENDOR_MAGIC: &[u8] = b"VNDRBOOT";
const PAGE_SIZE_AT: usize = 12;
const VENDOR_RAMDISK_SIZE_AT: usize = 24;
const DTB_SIZE_AT: usize = 2100;
const TABLE_SIZE_AT: usize = 2112;
const BOOTCONFIG_SIZE_AT: usize = 2124;
const HEADER_SIZE_AT: usize = 2096;

fn u32_at(b: &[u8], at: usize) -> Result<u32> {
    let s = b.get(at..at + 4).context("the image ends inside its header")?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn put_u32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn round_up(n: usize, page: usize) -> usize {
    n.div_ceil(page) * page
}

/// Whether the boot image `image` asks the kernel to start permissive.
pub fn boot_is_on(image: &[u8]) -> Result<bool> {
    Ok(boot_cmdline(image)?.split(' ').any(|w| w == KERNEL_ARG))
}

fn boot_cmdline(image: &[u8]) -> Result<String> {
    ensure!(image.starts_with(BOOT_MAGIC), "this is no boot image");
    let version = u32_at(image, BOOT_VERSION_AT)?;
    ensure!(version == 3 || version == 4, "boot header version {version}: only 3 and 4 are known");
    let raw = &image[BOOT_CMDLINE_AT..BOOT_CMDLINE_AT + BOOT_CMDLINE_LEN];
    let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
    Ok(String::from_utf8_lossy(&raw[..end]).into_owned())
}

/// Add or remove [`KERNEL_ARG`] in the boot image's command line, in the
/// header and nowhere else. `Ok(false)` where it was as wanted already.
pub fn boot(image: &mut [u8], on: bool) -> Result<bool> {
    let line = boot_cmdline(image)?;
    let mut words: Vec<&str> = line.split(' ').filter(|w| !w.is_empty()).collect();
    let had = words.contains(&KERNEL_ARG);
    if had == on {
        return Ok(false);
    }
    // An `enforcing=` of another value would contradict it.
    words.retain(|w| !w.starts_with("enforcing="));
    if on {
        words.push(KERNEL_ARG);
    }
    let new = words.join(" ");
    ensure!(new.len() < BOOT_CMDLINE_LEN, "the boot image's command line has no room for {KERNEL_ARG}");
    let field = &mut image[BOOT_CMDLINE_AT..BOOT_CMDLINE_AT + BOOT_CMDLINE_LEN];
    field.fill(0);
    field[..new.len()].copy_from_slice(new.as_bytes());
    Ok(true)
}

/// Where the bootconfig section of a version 4 vendor_boot image starts,
/// how long it is, and the page it is padded to.
fn bootconfig_at(image: &[u8]) -> Result<(usize, usize, usize)> {
    ensure!(image.starts_with(VENDOR_MAGIC), "this is no vendor_boot image");
    let version = u32_at(image, 8)?;
    ensure!(version == 4, "vendor_boot header version {version}: bootconfig needs version 4");
    let page = u32_at(image, PAGE_SIZE_AT)? as usize;
    ensure!(page.is_power_of_two() && page >= 2048, "vendor_boot page size {page}");
    let at = round_up(u32_at(image, HEADER_SIZE_AT)? as usize, page)
        + round_up(u32_at(image, VENDOR_RAMDISK_SIZE_AT)? as usize, page)
        + round_up(u32_at(image, DTB_SIZE_AT)? as usize, page)
        + round_up(u32_at(image, TABLE_SIZE_AT)? as usize, page);
    let len = u32_at(image, BOOTCONFIG_SIZE_AT)? as usize;
    ensure!(at + len <= image.len(), "the vendor_boot image ends inside its bootconfig");
    Ok((at, len, page))
}

/// Whether the vendor_boot image's bootconfig asks for permissive.
pub fn vendor_boot_is_on(image: &[u8]) -> Result<bool> {
    let (at, len, _) = bootconfig_at(image)?;
    Ok(String::from_utf8_lossy(&image[at..at + len]).lines().any(|l| l.trim() == BOOTCONFIG))
}

/// Add or remove [`BOOTCONFIG`] in the vendor_boot image's bootconfig,
/// within the page its section is padded to, so nothing after it moves.
pub fn vendor_boot(image: &mut [u8], on: bool) -> Result<bool> {
    let (at, len, page) = bootconfig_at(image)?;
    let text = String::from_utf8_lossy(&image[at..at + len]).into_owned();
    let had = text.lines().any(|l| l.trim() == BOOTCONFIG);
    if had == on {
        return Ok(false);
    }
    let mut lines: Vec<&str> = text
        .lines()
        .filter(|l| {
            let key = l.split('=').next().unwrap_or("").trim();
            key != BOOTCONFIG_KEY
        })
        .collect();
    if on {
        lines.push(BOOTCONFIG);
    }
    let mut new = lines.join("\n");
    if !new.is_empty() {
        new.push('\n');
    }
    let room = round_up(len.max(1), page);
    ensure!(new.len() <= room && at + room <= image.len(), "the vendor_boot image's bootconfig has no room for {BOOTCONFIG}");
    image[at..at + room].fill(0);
    image[at..at + new.len()].copy_from_slice(new.as_bytes());
    put_u32(image, BOOTCONFIG_SIZE_AT, new.len() as u32);
    Ok(true)
}

/// Whether the init_boot image's ramdisk starts Skyward's program first.
pub fn init_boot_is_on(image: &[u8]) -> Result<bool> {
    let parsed = BootImage::parse(image).context("read the init_boot image")?;
    let rd = Ramdisk::decode(&parsed.ramdisk).context("read the init_boot ramdisk")?;
    Ok(rd.find(OWN_INIT).is_some())
}

/// The init_boot image `image` with `program` as its ramdisk's first
/// program and its own kept as [`OWN_INIT`], or, with `on` false, its own
/// put back. `None` where it was as wanted already. The image returned
/// fits within `room` bytes, the partition's size less its AVB footer.
pub fn init_boot(image: &[u8], program: &[u8], on: bool, room: usize) -> Result<Option<Vec<u8>>> {
    let mut parsed = BootImage::parse(image).context("read the init_boot image")?;
    let mut rd = Ramdisk::decode(&parsed.ramdisk).context("read the init_boot ramdisk")?;
    let has = rd.find(OWN_INIT).is_some();
    match (has, on) {
        (true, true) => {
            // A newer program takes the place of the one there.
            if rd.find(INIT).map(|e| e.data.as_slice()) == Some(program) {
                return Ok(None);
            }
            rd.replace(INIT, program.to_vec())?;
        }
        (false, false) => return Ok(None),
        (false, true) => {
            let own = rd.entries.iter_mut().find(|e| e.name == INIT).context("the init_boot ramdisk has no init")?;
            ensure!(own.mode & 0o170000 == 0o100000, "the init_boot ramdisk's init is not a regular file");
            let mut ours = own.clone();
            own.name = OWN_INIT.to_string();
            ours.data = program.to_vec();
            rd.entries.push(ours);
        }
        (true, false) => {
            rd.entries.retain(|e| e.name != INIT);
            match rd.entries.iter_mut().find(|e| e.name == OWN_INIT) {
                Some(e) => e.name = INIT.to_string(),
                None => bail!("the init_boot ramdisk lost its own init"),
            }
        }
    }
    parsed.ramdisk = lz4_legacy(&rd.archive()?);
    let out = parsed.to_bytes()?;
    ensure!(out.len() <= room, "the init_boot image grows to {} bytes, past the {room} its partition has", out.len());
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vendor(bootconfig: &str) -> Vec<u8> {
        let page = 4096;
        let mut b = vec![0u8; page * 3];
        b[..8].copy_from_slice(VENDOR_MAGIC);
        put_u32(&mut b, 8, 4);
        put_u32(&mut b, PAGE_SIZE_AT, page as u32);
        put_u32(&mut b, HEADER_SIZE_AT, 2128);
        put_u32(&mut b, VENDOR_RAMDISK_SIZE_AT, 10);
        put_u32(&mut b, BOOTCONFIG_SIZE_AT, bootconfig.len() as u32);
        b[page * 2..page * 2 + bootconfig.len()].copy_from_slice(bootconfig.as_bytes());
        b
    }

    #[test]
    fn bootconfig_gains_and_loses_the_line_and_keeps_the_rest() {
        let start = "androidboot.hardware=qcom\nandroidboot.memcg=1\n";
        let mut b = vendor(start);
        assert!(vendor_boot(&mut b, true).unwrap());
        assert!(vendor_boot_is_on(&b).unwrap());
        assert!(!vendor_boot(&mut b, true).unwrap());
        assert!(vendor_boot(&mut b, false).unwrap());
        assert_eq!(b, vendor(start));
    }

    #[test]
    fn boot_command_line_gains_and_loses_the_word() {
        let mut b = vec![0u8; 4096];
        b[..8].copy_from_slice(BOOT_MAGIC);
        put_u32(&mut b, BOOT_VERSION_AT, 4);
        let orig = b.clone();
        assert!(boot(&mut b, true).unwrap());
        assert!(boot_is_on(&b).unwrap());
        assert!(boot(&mut b, false).unwrap());
        assert_eq!(b, orig);
    }

    /// The phone's own images, where a test run has them at hand.
    #[test]
    fn the_phones_own_images_change_and_change_back() {
        if let Ok(mut v) = std::fs::read("/tmp/vendor_boot.img") {
            let orig = v.clone();
            assert!(vendor_boot(&mut v, true).unwrap());
            assert!(vendor_boot_is_on(&v).unwrap());
            assert!(vendor_boot(&mut v, false).unwrap());
            assert_eq!(v, orig);
        }
        if let Ok(i) = std::fs::read("/tmp/init_boot.img") {
            let on = init_boot(&i, b"PROGRAM", true, i.len()).unwrap().unwrap();
            assert!(init_boot_is_on(&on).unwrap());
            let off = init_boot(&on, b"PROGRAM", false, i.len()).unwrap().unwrap();
            assert!(!init_boot_is_on(&off).unwrap());
            let a = Ramdisk::decode(&BootImage::parse(&i).unwrap().ramdisk).unwrap();
            let b = Ramdisk::decode(&BootImage::parse(&off).unwrap().ramdisk).unwrap();
            assert_eq!(a.entries.len(), b.entries.len());
            for (x, y) in a.entries.iter().zip(&b.entries) {
                assert_eq!((&x.name, x.mode, &x.data), (&y.name, y.mode, &y.data));
            }
        }
    }
}
