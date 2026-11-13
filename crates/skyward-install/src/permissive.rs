//! Turn the permissive kernel on or off in one slot: `enforcing=0` in its
//! boot image, `androidboot.selinux=permissive` in its vendor_boot image,
//! and Skyward's first program in its init_boot ramdisk, as
//! [`skyward_bootimg::permissive`] describes.
//!
//! All three images are read and changed in memory before any is written,
//! so a slot whose images cannot all be changed is left as it was. Each is
//! written in place, its AVB footer and vbmeta left where they are, and
//! read back.

use crate::image::device;
use anyhow::{ensure, Context, Result};
use skyward_bootimg::permissive;
use skyward_vab::Slot;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// The program the recovery carries for init_boot ramdisks.
pub fn program() -> PathBuf {
    Path::new("/").join(skyward_config::ramdisk::PERMISSIVE_INIT)
}

/// How the three partitions of a slot stand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct State {
    pub boot: bool,
    pub vendor_boot: bool,
    pub init_boot: bool,
}

impl State {
    /// On only where all three are.
    pub fn on(self) -> bool {
        self.boot && self.vendor_boot && self.init_boot
    }
}

fn read_whole(path: &Path) -> Result<Vec<u8>> {
    let mut f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut b = Vec::new();
    f.read_to_end(&mut b).with_context(|| format!("read {}", path.display()))?;
    Ok(b)
}

/// Where the image data of a partition may run to: up to its vbmeta where
/// it ends in an AVB footer, the whole partition where it does not.
pub fn room(partition: &[u8]) -> usize {
    const FOOTER: usize = 64;
    if partition.len() < FOOTER {
        return partition.len();
    }
    let footer = &partition[partition.len() - FOOTER..];
    if &footer[..4] != b"AVBf" {
        return partition.len();
    }
    let vbmeta_at = u64::from_be_bytes(footer[20..28].try_into().expect("eight bytes")) as usize;
    vbmeta_at.min(partition.len() - FOOTER)
}

/// How the permissive kernel stands in `slot`, among the links in `dir`.
pub fn state(dir: &Path, slot: Slot) -> Result<State> {
    Ok(State {
        boot: permissive::boot_is_on(&read_whole(&device(dir, "boot", slot))?)?,
        vendor_boot: permissive::vendor_boot_is_on(&read_whole(&device(dir, "vendor_boot", slot))?)?,
        init_boot: permissive::init_boot_is_on(&read_whole(&device(dir, "init_boot", slot))?)?,
    })
}

/// Turn the permissive kernel on or off in `slot`, among the links in
/// `dir`, with `program` as init_boot's first program. Returns the
/// partitions it wrote; none where the slot was as wanted already.
pub fn set(dir: &Path, slot: Slot, on: bool, program: &[u8]) -> Result<Vec<String>> {
    let mut writes: Vec<(PathBuf, Vec<u8>, usize)> = Vec::new();

    let path = device(dir, "boot", slot);
    let mut boot = read_whole(&path)?;
    if permissive::boot(&mut boot, on).context("change the boot image")? {
        let len = boot.len();
        writes.push((path, boot, len));
    }

    let path = device(dir, "vendor_boot", slot);
    let mut vendor = read_whole(&path)?;
    if permissive::vendor_boot(&mut vendor, on).context("change the vendor_boot image")? {
        let len = vendor.len();
        writes.push((path, vendor, len));
    }

    let path = device(dir, "init_boot", slot);
    let init = read_whole(&path)?;
    let room = room(&init);
    if let Some(mut new) = permissive::init_boot(&init, program, on, room).context("change the init_boot image")? {
        // What the old image held past the new one's end is cleared, up to
        // its vbmeta, so nothing stale is left in the partition.
        new.resize(room, 0);
        writes.push((path, new, room));
    }

    let mut written = Vec::new();
    for (path, data, len) in &writes {
        write_back(path, &data[..*len])?;
        written.push(path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    }
    Ok(written)
}

/// Write `data` over the start of `path` and read it back.
fn write_back(path: &Path, data: &[u8]) -> Result<()> {
    let mut f = OpenOptions::new().read(true).write(true).open(path).with_context(|| format!("open {} to write", path.display()))?;
    f.write_all(data).with_context(|| format!("write {}", path.display()))?;
    f.sync_all().with_context(|| format!("sync {}", path.display()))?;
    f.seek(SeekFrom::Start(0))?;
    let mut back = vec![0u8; data.len()];
    f.read_exact(&mut back).with_context(|| format!("read {} back", path.display()))?;
    ensure!(back == data, "{} reads back other than written", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_stops_at_the_vbmeta_a_footer_names() {
        let mut p = vec![0u8; 8192];
        assert_eq!(room(&p), 8192);
        let at = p.len() - 64;
        p[at..at + 4].copy_from_slice(b"AVBf");
        p[at + 20..at + 28].copy_from_slice(&4096u64.to_be_bytes());
        assert_eq!(room(&p), 4096);
    }
}
