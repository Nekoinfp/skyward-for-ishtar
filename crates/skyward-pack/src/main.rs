use anyhow::{ensure, Context, Result};
use clap::{Parser, Subcommand};
use sha2::{Digest, Sha256};
use skyward_config::ramdisk as paths;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

mod avb;

use skyward_bootimg::{BootImage, Ramdisk};

/// Builds Skyward recovery images from a device's own stock images.
#[derive(Parser)]
#[command(name = "skyward-pack")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
// The commands carry the paths and lists they were given; the largest is a
// few hundred bytes, which is the one copy clap parses into either way.
#[allow(clippy::large_enum_variant)]
enum Cmd {
    /// Print an image's header and the files in its ramdisk.
    List { image: PathBuf },
    /// Write one ramdisk file to stdout.
    Cat { image: PathBuf, path: String },
    /// Unpack and pack an image's ramdisk unchanged, to check that a rebuilt
    /// archive keeps every entry.
    Repack { image: PathBuf, out: PathBuf },
    /// Join a kernel and another image's ramdisk into one boot image.
    Boot {
        /// Image holding the kernel (the device's boot partition).
        #[arg(long)]
        kernel: PathBuf,
        /// Image holding the ramdisk.
        #[arg(long)]
        ramdisk: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Build a Skyward recovery image on top of the device's stock recovery:
    /// its init, adbd, SELinux policy and libraries stay, the recovery
    /// service becomes Skyward. The image fills the whole partition and ends
    /// in an unsigned AVB footer of its own.
    Recovery {
        /// The device's stock recovery image: the whole partition, as the
        /// phone or a full update package holds it, ending in its AVB footer.
        #[arg(long)]
        stock: PathBuf,
        /// Skyward built for the device (aarch64-linux-android).
        #[arg(long)]
        binary: PathBuf,
        /// The first program of a system installed beside the installed
        /// one, skyward-system-init built for the device and linked
        /// statically.
        #[arg(long)]
        system_init: PathBuf,
        /// The first program of an init_boot ramdisk made permissive,
        /// skyward-permissive-init built for the device and linked
        /// statically.
        #[arg(long)]
        permissive_init: PathBuf,
        /// The device configuration, devices/<codename>.toml.
        #[arg(long)]
        config: PathBuf,
        /// The UI typeface.
        #[arg(long)]
        font: PathBuf,
        /// The terminal's monospaced typeface.
        #[arg(long)]
        mono: PathBuf,
        /// The monospaced typeface's licence, which goes with it.
        #[arg(long)]
        mono_license: PathBuf,
        /// A firmware file to put where the kernel's own loader finds it
        /// before its first driver probe: name=local-file. The loader reads
        /// /lib/firmware before it asks anyone else, and the motor's driver
        /// probes in early-init, before a recovery starts, so that is where
        /// the file has to be.
        #[arg(long = "firmware")]
        firmware: Vec<String>,
        /// A module to put in the recovery's module directory, as
        /// name.ko=local-file. It must match the recovery kernel, not merely
        /// the installed system's vendor_dlkm.
        #[arg(long = "module")]
        modules: Vec<String>,
        /// A folder holding se_omapi as system/bin/se_omapi and its service
        /// description as se_omapi.xml.
        ///
        /// A phone whose weaver HAL reaches the secure element through
        /// Android's own OMAPI service gets nothing back from it in a
        /// recovery, which runs no such service: every call to the weaver
        /// then fails, and a user's secret behind a weaver slot cannot be
        /// read. se_omapi is that service on its own; this puts it in the
        /// ramdisk, for the configuration to start.
        #[arg(long)]
        omapi: Option<PathBuf>,
        /// The phone's own snapshot daemon, which serves the blocks of a
        /// compressed snapshot. It has to be in the ramdisk: the partition it
        /// is needed for is the system one, so it cannot be taken from there.
        #[arg(long)]
        snapuserd: Option<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Print the stock build a device's recovery is made on, as lines of
    /// name=value: its version, where its full update package is, and the
    /// SHA-256 of the recovery partition in that package. Given somewhere
    /// to write it, also take that partition out of the package on its
    /// server, reading only what the partition takes, and write it there
    /// once its SHA-256 is that one.
    Stock {
        /// The device configuration, devices/<codename>.toml.
        #[arg(long)]
        config: PathBuf,
        /// Where to write the stock recovery image.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Another partition to take from the same package, such as odm.
        /// It is written beside --out, named <partition>.img.
        #[arg(long, requires = "out")]
        partition: Option<String>,
    },
    /// Hold a device configuration against the partitions a phone ships: every
    /// program, library folder and firmware file it names under /vendor or
    /// /odm has to be in the tree it is taken from. The trees are partitions
    /// taken out of the phone's package and mounted, so that a configuration
    /// can be checked against a build before anything is flashed.
    ///
    /// Paths under /system are not checked: they are the recovery's own, put
    /// there by the packing rather than taken from the phone.
    Paths {
        /// The device configuration, devices/<codename>.toml.
        #[arg(long)]
        config: PathBuf,
        /// A mounted or unpacked vendor partition.
        #[arg(long)]
        vendor: Option<PathBuf>,
        /// A mounted or unpacked odm partition.
        #[arg(long)]
        odm: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::List { image } => list(&image),
        Cmd::Cat { image, path } => {
            let rd = Ramdisk::decode(&BootImage::read(&image)?.ramdisk)?;
            let e = rd.find(&path).with_context(|| format!("{path} is not in the ramdisk"))?;
            std::io::stdout().write_all(&e.data)?;
            Ok(())
        }
        Cmd::Repack { image, out } => {
            let mut img = BootImage::read(&image)?;
            img.ramdisk = Ramdisk::decode(&img.ramdisk)?.encode()?;
            write(&out, &img, &img.to_bytes()?)
        }
        Cmd::Boot { kernel, ramdisk, out } => {
            let mut img = BootImage::read(&ramdisk)?;
            img.kernel = BootImage::read(&kernel)?.kernel;
            write(&out, &img, &img.to_bytes()?)
        }
        Cmd::Recovery { stock, binary, system_init, permissive_init, config, font, mono, mono_license, firmware, modules, omapi, snapuserd, out } => {
            let size = partition_size(&stock)?;
            let mut img = BootImage::read(&stock)?;
            let mut rd = Ramdisk::decode(&img.ramdisk)?;
            // A stock recovery ramdisk has no third-party tree; refuse to build
            // on top of another custom recovery.
            if let Some(e) = rd.entries.iter().find(|e| e.name == "FFiles" || e.name.starts_with("twres")) {
                anyhow::bail!("{} holds {}, which is not a stock recovery", stock.display(), e.name);
            }
            let read = |p: &PathBuf| std::fs::read(p).with_context(|| format!("read {}", p.display()));
            skyward_config::DeviceConfig::load(&config)?;
            rd.replace(paths::RECOVERY, read(&binary)?)?;
            // Install scripts look for the commands a stock recovery lacks
            // among its programs, and its unzip there too, a link to ziptool
            // until it points here; the recovery binary is each of them.
            let (_, recovery) = paths::RECOVERY.rsplit_once('/').expect("the recovery binary is in a folder");
            for command in paths::COMMANDS {
                rd.link(command, recovery)?;
            }
            // And the recovery's own commands need a name of their own: what
            // this binary does with its arguments is decided by the name it
            // was started as, so `recovery` starts the service and only
            // another name reaches `info`, `check`, `lp` and the rest.
            rd.link(paths::TOOL, recovery)?;
            // The second system's first program: it runs before any library
            // is there to load, so it is given a place of its own.
            put(&mut rd, paths::SYSTEM_INIT, 0o755, read(&system_init)?)?;
            put(&mut rd, paths::PERMISSIVE_INIT, 0o755, read(&permissive_init)?)?;
            put(&mut rd, paths::CONFIG, 0o644, read(&config)?)?;
            put(&mut rd, paths::FONT, 0o644, read(&font)?)?;
            put(&mut rd, paths::MONO, 0o644, read(&mono)?)?;
            put(&mut rd, paths::MONO_LICENSE, 0o644, read(&mono_license)?)?;
            for file in firmware {
                let (name, from) = file.split_once('=').with_context(|| "--firmware takes name=local-file")?;
                ensure!(!name.is_empty() && !name.starts_with('/') && !name.contains(".."), "{name} is not a firmware name");
                put(&mut rd, &format!("lib/firmware/{name}"), 0o644, read(&PathBuf::from(from))?)?;
            }
            for file in modules {
                let (name, from) = file.split_once('=').with_context(|| "--module takes name.ko=local-file")?;
                ensure!(name.ends_with(".ko") && !name.contains('/') && !name.contains(".."), "{name} is not a module name");
                put(&mut rd, &format!("lib/modules/{name}"), 0o644, read(&PathBuf::from(from))?)?;
            }
            // A stock ramdisk may run logd without any scheduling profiles,
            // and logd stops as soon as it cannot find the one it asks for.
            if rd.find(paths::TASK_PROFILES).is_none() {
                rd.add(paths::TASK_PROFILES, 0o644, TASK_PROFILES.to_vec())?;
            }
            put(&mut rd, paths::MTP_RC, 0o644, MTP_RC.to_vec())?;
            // The OMAPI service the weaver reaches a secure element through,
            // and the description the service directory wants of it. The
            // recovery starts it with the phone's other services.
            if let Some(stage) = &omapi {
                install_omapi(&mut rd, stage, &read)?;
            }
            // Beside the recovery itself, where the mapping looks for it
            // before any partition is mounted.
            if let Some(daemon) = &snapuserd {
                let bytes = std::fs::read(daemon).with_context(|| format!("read {}", daemon.display()))?;
                rd.add("system/bin/snapuserd", 0o755, bytes)?;
            }
            // On an unlocked device adbd keeps root when ro.secure is 0 and
            // lets any computer in without authorization when ro.adb.secure is
            // 0; init lets a later line of a property file override an earlier
            // one. update_engine refuses a package built before the recovery
            // itself unless both of the last two are set; the recovery checks
            // packages against the installed system instead.
            let stock_props = &rd
                .find(paths::PROP_DEFAULT)
                .with_context(|| format!("{} is not in the ramdisk", paths::PROP_DEFAULT))?
                .data;
            let mut props = without(stock_props, &BUILD_VERSION);
            props.extend_from_slice(b"\nro.secure=0\nro.adb.secure=0\nro.debuggable=1\nro.ota.allow_downgrade=true\n");
            rd.replace(paths::PROP_DEFAULT, props)?;
            img.ramdisk = rd.encode()?;
            let bytes = img.to_bytes()?;
            // The bootloader gives the secure world the version and patch
            // level the stock footer names; the system's keys were made
            // under those, so the recovery's footer names the same.
            let stock_props = avb::properties(&std::fs::read(&stock).with_context(|| format!("read {}", stock.display()))?);
            for (key, value, _) in &stock_props {
                println!("{key}={value}");
            }
            if !stock_props.iter().any(|(key, _, _)| key.ends_with(".security_patch")) {
                // Both of this phone's stock recoveries — the one OS4.0.0.18
                // ships and the one OS4.0.4.0 ships — put nothing in their
                // footer but a fingerprint, so this is how the phone is built
                // rather than something the package lost. The recovery sets
                // those properties from the installed system as it starts.
                eprintln!(
                    "note: {} names no security patch in its footer, as this phone's stock recoveries do not either; the recovery reads it from the installed system",
                    stock.display()
                );
            }
            let props: Vec<u8> = stock_props.into_iter().flat_map(|(_, _, d)| d).collect();
            write(&out, &img, &avb::footed(&bytes, "recovery", size, &Sha256::digest(&bytes), &props)?)
        }
        Cmd::Stock { config, out, partition } => {
            let stock = skyward_config::DeviceConfig::load(&config)?.stock.with_context(|| format!("{} names no stock build", config.display()))?;
            println!("version={}\npackage={}\nrecovery={}", stock.version, stock.package, stock.recovery);
            match (out, partition) {
                (Some(out), partition) => take_stock_recovery(&stock, &out, partition.as_deref()),
                (None, _) => Ok(()),
            }
        }
        Cmd::Paths { config, vendor, odm } => {
            let trees = [("vendor", vendor), ("odm", odm)];
            check_paths(&config, &trees)
        }
    }
}

/// The parts of a phone a path in a configuration can be taken from. What the
/// recovery carries itself is not one of them: a path under `/system` is put
/// there by the packing, not by the phone.
const TREES: [&str; 2] = ["vendor", "odm"];

/// Every program, library folder and firmware file the configuration names,
/// held against the trees given for them: the partitions it takes them from,
/// mounted from the phone's own package.
///
/// A path whose tree was not given is said to be not checked rather than
/// passed over, since a half-checked configuration is not a checked one.
fn check_paths(config: &Path, trees: &[(&str, Option<PathBuf>)]) -> Result<()> {
    let cfg = skyward_config::DeviceConfig::load(config)?;
    let mut named: Vec<(&str, &str)> = Vec::new();
    let (mut looked, mut missing) = (0, 0);
    for service in &cfg.services {
        named.push((&service.path, "program"));
        for library in &service.libraries {
            named.push((library, "library folder"));
        }
    }
    if let Some(wlan) = &cfg.wlan {
        named.push((&wlan.supplicant, "program"));
        for file in &wlan.firmware {
            if file.path.starts_with('/') {
                named.push((&file.path, "firmware"));
            }
        }
    }
    if let Some(haptics) = &cfg.haptics {
        for file in &haptics.firmware {
            if file.path.starts_with('/') {
                named.push((&file.path, "firmware"));
            }
        }
    }
    let mut ours: Vec<&str> = Vec::new();
    let mut unchecked: Vec<&str> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for (path, kind) in named {
        // Several programs share a library folder; it is looked for once.
        if seen.contains(&path) {
            continue;
        }
        seen.push(path);
        let Some((tree, rest)) = TREES.iter().find_map(|tree| path.strip_prefix(&format!("/{tree}/")).map(|rest| (*tree, rest))) else {
            if path.starts_with("/system/") {
                ours.push(path);
            }
            continue;
        };
        let Some(dir) = trees.iter().find(|(name, _)| *name == tree).and_then(|(_, dir)| dir.as_ref()) else {
            println!("  not checked  {path}  (no {tree} partition given)");
            unchecked.push(path);
            continue;
        };
        looked += 1;
        if dir.join(rest).exists() {
            println!("  there        {path}");
        } else {
            println!("  MISSING      {path}  ({kind})");
            missing += 1;
        }
    }
    if !ours.is_empty() {
        println!("\nthe recovery's own, which the packing puts there:");
        for path in ours {
            println!("  {path}");
        }
    }
    println!("\n{looked} paths checked against the partitions given, {missing} missing, {} not checked", unchecked.len());
    ensure!(
        unchecked.is_empty(),
        "{} paths are in partitions that were not given, so the configuration is only half checked: give {}",
        unchecked.len(),
        trees.iter().filter(|(_, dir)| dir.is_none()).map(|(name, _)| format!("--{name}")).collect::<Vec<_>>().join(" and ")
    );
    ensure!(missing == 0, "{missing} of the paths {} names are in none of the partitions given", config.display());
    Ok(())
}

/// The partition a recovery is made on, as a full update package names it.
const RECOVERY: &str = "recovery";

/// Refuse a name that is not a partition of its own.
///
/// The name becomes a path beside `--out`, so nothing in it may lead
/// anywhere else, and the recovery partition is taken out already: asking for
/// it twice would write it over itself.
fn separate_partition(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-') && name != RECOVERY,
        "{name} is not a separate partition name: give one such as odm, which is written beside --out"
    );
    Ok(())
}

/// Take the recovery partition out of the stock build's full update package
/// on its server, reading only what that partition takes of it, and write it
/// to `out` once its SHA-256 is the one the configuration names.
fn take_stock_recovery(stock: &skyward_config::Stock, out: &Path, other: Option<&str>) -> Result<()> {
    if let Some(name) = other {
        separate_partition(name)?;
    }
    let parent = out.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).with_context(|| format!("make {}", parent.display()))?;
    // Taken out beside where it goes, so it is moved there whole.
    let work = tempfile::Builder::new().prefix(".stock").tempdir_in(parent).with_context(|| format!("make a folder in {}", parent.display()))?;
    let payload = unpayload::Payload::open_url(&stock.package).with_context(|| format!("open {}", stock.package))?;
    let partitions = std::iter::once(RECOVERY.into()).chain(other.map(str::to_owned)).collect();
    let options = unpayload::ExtractOptions { output_dir: work.path().to_path_buf(), partitions, ..Default::default() };
    let report = |event: unpayload::Event<'_>| match event {
        unpayload::Event::Started { partitions } => {
            for (name, bytes) in partitions {
                println!("taking {name} out of the package: {bytes} bytes");
            }
        }
        unpayload::Event::Extracted(image) => println!("took {} out: {} bytes", image.partition, image.size),
        unpayload::Event::Progress { .. } => {}
    };
    let taken = payload.extract(&options, &report).with_context(|| format!("take {RECOVERY} out of {}", stock.package))?;
    let image = taken.iter().find(|i| i.partition == RECOVERY).with_context(|| format!("{} holds no {RECOVERY} partition", stock.package))?;
    let sum: String = image.sha256.as_deref().context("the package's own sum of the image was not checked")?.iter().map(|b| format!("{b:02x}")).collect();
    ensure!(sum == stock.recovery, "the {RECOVERY} partition in {} has the SHA-256 {sum}, where the configuration names {}", stock.package, stock.recovery);
    std::fs::rename(&image.path, out).with_context(|| format!("move {} to {}", image.path.display(), out.display()))?;
    if let Some(partition) = other {
        let extra = taken.iter().find(|i| i.partition == partition).with_context(|| format!("{} holds no {partition} partition", stock.package))?;
        let target = out.parent().unwrap_or(Path::new(".")).join(format!("{partition}.img"));
        std::fs::rename(&extra.path, &target).with_context(|| format!("move {} to {}", extra.path.display(), target.display()))?;
        println!("wrote {} ({} bytes)", target.display(), extra.size);
    }
    println!("wrote {} ({} bytes), its SHA-256 the one the configuration names", out.display(), image.size);
    Ok(())
}

/// The size of the partition `stock` fills: all of it, as the phone and a
/// full update package hold it, which its AVB footer ends.
fn partition_size(stock: &Path) -> Result<usize> {
    let mut file = File::open(stock).with_context(|| format!("open {}", stock.display()))?;
    let size = file.metadata().with_context(|| format!("stat {}", stock.display()))?.len();
    let mut magic = [0u8; 4];
    if size >= 64 {
        file.seek(SeekFrom::Start(size - 64)).with_context(|| format!("seek in {}", stock.display()))?;
        file.read_exact(&mut magic).with_context(|| format!("read {}", stock.display()))?;
    }
    ensure!(
        &magic == b"AVBf",
        "{} does not end in an AVB footer; give the whole recovery partition, as the phone or a full update package holds it",
        stock.display()
    );
    Ok(usize::try_from(size)?)
}

/// What a recovery image says about the build it is: the version and patch
/// levels of its own ramdisk's system and vendor partition, which are older
/// than those installed beside it whenever the phone has taken an update.
/// The phone's secure services read these to tell the secure world which
/// build is running, and keys the installed system made are refused when
/// they say the wrong one, so they are left out here for the recovery to
/// fill in from the installed system once it has that mounted.
const BUILD_VERSION: [&str; 4] = [
    "ro.build.version.release",
    "ro.build.version.release_or_codename",
    "ro.build.version.security_patch",
    "ro.vendor.build.security_patch",
];

/// The scheduling profiles a recovery's programs look up by name. A
/// recovery's cgroup setup mounts no controllers, so of all a system's
/// profiles do, only the timer slack applies here.
const TASK_PROFILES: &[u8] = include_bytes!("task_profiles.json");

/// The init rules that give the USB port file transfer beside adb, which a
/// stock recovery does not offer.
const MTP_RC: &[u8] = include_bytes!("mtp.rc");

/// The OMAPI service a weaver reaches a secure element through, and the
/// description that lets it register.
///
/// The service registers `android.se.omapi.ISecureElementService/default`,
/// which is the name the Thales weaver HAL looks up. Android's own lives in
/// system_server; a recovery runs no framework, so it gets its own.
///
/// No init rule starts it: the recovery starts it with the phone's other
/// services, once the vendor partition is mounted, since it loads its two
/// interface libraries from there and asks the secure element HAL for eSE1.
/// The phone's configuration names it after the secure element and before
/// the weaver. The service directory turns away a name no description
/// declares, so the description goes where the directory reads them at boot.
/// The rest of what it loads, libbinder_ndk and below, is the stock
/// recovery's own: a newer build of the interface libraries asks libbinder_ndk
/// for symbols the stock one lacks.
const OMAPI_FILES: [(&str, &str, u32); 6] = [
    ("system/bin/se_omapi", "system/bin/se_omapi", 0o755),
    ("se_omapi.xml", "system/etc/vintf/manifest/se_omapi.xml", 0o644),
    // The Thales weaver's own description: an odm that carries the weaver
    // without one (the ColorOS port's) has the directory turn it away.
    ("weaver-thales.xml", "system/etc/vintf/manifest/weaver-thales.xml", 0o644),
    // se_omapi's two AIDL libraries, which the recovery does not carry; the
    // copies are built for API 36 and patched to the recovery's API 35
    // libbinder_ndk, with the one API 36 call answered by a no-op shim.
    ("system/lib64/android.se.omapi-V1-ndk.so", "system/lib64/android.se.omapi-V1-ndk.so", 0o644),
    ("system/lib64/android.hardware.secure_element-V1-ndk.so", "system/lib64/android.hardware.secure_element-V1-ndk.so", 0o644),
    ("system/lib64/libbinder_ndk_shim36.so", "system/lib64/libbinder_ndk_shim36.so", 0o644),
];

/// Put the OMAPI service and its description into `rd`, from the folder
/// `stage` holds them in.
///
/// A stock recovery that already carries the service keeps its own: the two
/// would answer to the same name, and the second to register would replace
/// the first.
fn install_omapi(rd: &mut Ramdisk, stage: &Path, read: &impl Fn(&PathBuf) -> Result<Vec<u8>>) -> Result<()> {
    let at = |name: &str| stage.join(name.replace('/', std::path::MAIN_SEPARATOR_STR));
    let stock_omapi = rd.find(OMAPI_FILES[0].1).is_some();
    for (from, to, mode) in OMAPI_FILES {
        if rd.find(to).is_some() || (stock_omapi && !(from == "weaver-thales.xml" || from.starts_with("system/lib64/"))) {
            continue;
        }
        let bytes = read(&at(from)).with_context(|| format!("--omapi: {} is not in {}", from, stage.display()))?;
        rd.add(to, mode, bytes)?;
    }
    Ok(())
}

/// A property file without the lines that set any of `names`.
fn without(props: &[u8], names: &[&str]) -> Vec<u8> {
    let mut out = Vec::with_capacity(props.len());
    for line in props.split_inclusive(|b| *b == b'\n') {
        let key = line.split(|b| *b == b'=').next().unwrap_or_default();
        if !names.iter().any(|n| n.as_bytes() == key) {
            out.extend_from_slice(line);
        }
    }
    out
}

/// Put a regular file into `rd`: a file already there has its contents
/// replaced, so a build can be made on top of an image made before it.
fn put(rd: &mut Ramdisk, name: &str, mode: u32, data: Vec<u8>) -> Result<()> {
    if rd.find(name).is_some() {
        rd.replace(name, data)
    } else {
        rd.add(name, mode, data)
    }
}

/// Write `bytes`, the image `img` makes, to `out`.
fn write(out: &Path, img: &BootImage, bytes: &[u8]) -> Result<()> {
    std::fs::write(out, bytes).with_context(|| format!("write {}", out.display()))?;
    println!(
        "wrote {} ({} bytes kernel, {} bytes ramdisk, {} bytes in all)",
        out.display(),
        img.kernel.len(),
        img.ramdisk.len(),
        bytes.len()
    );
    Ok(())
}

fn list(image: &Path) -> Result<()> {
    let img = BootImage::read(image)?;
    let h = &img.header;
    println!(
        "header v{}  kernel {} bytes  ramdisk {} bytes  {:?}",
        h.header_version(),
        img.kernel.len(),
        img.ramdisk.len(),
        h.osversionpatch
    );
    if img.ramdisk.is_empty() {
        return Ok(());
    }
    let rd = Ramdisk::decode(&img.ramdisk)?;
    for e in &rd.entries {
        println!("{:06o} {:>5}:{:<5} {:>10}  {}", e.mode, e.uid, e.gid, e.data.len(), e.name);
    }
    println!("{} entries", rd.entries.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lines_that_set_a_name_are_the_only_ones_left_out() {
        let props = b"ro.build.id=TQ\nro.build.version.release=13\nro.build.version.release_or_codename=13\nro.product.name=ishtar\n\
ro.build.version.security_patch=2026-10-01\nro.vendor.build.security_patch=2026-10-01\nro.vendor.build.security_patch_level=1\n";
        let out = without(props, &BUILD_VERSION);
        let text = String::from_utf8(out).expect("property files are text");
        assert!(!text.contains("ro.build.version.release=13"));
        assert!(!text.contains("ro.build.version.release_or_codename=13"), "the newer name for the release is left out too");
        assert!(!text.contains("ro.build.version.security_patch=2026-10-01"));
        assert!(!text.contains("ro.vendor.build.security_patch=2026-10-01"), "the vendor partition's patch level is left out too");
        assert!(text.contains("ro.vendor.build.security_patch_level=1"), "a longer name is left alone");
        assert!(text.contains("ro.build.id=TQ") && text.contains("ro.product.name=ishtar"));
    }

    #[test]
    fn the_profiles_name_all_the_recovery_asks_for() {
        let profiles: serde_json::Value = serde_json::from_slice(TASK_PROFILES).expect("the profiles are JSON");
        let names = |key: &str| -> Vec<String> {
            profiles[key].as_array().expect("a list").iter().map(|p| p["Name"].as_str().expect("a name").to_string()).collect()
        };
        let (plain, aggregate) = (names("Profiles"), names("AggregateProfiles"));
        // What libprocessgroup asks for when a program sets its scheduling
        // policy, and what the recovery's own init scripts give logd.
        let asked = [
            "SCHED_SP_DEFAULT",
            "SCHED_SP_BACKGROUND",
            "SCHED_SP_FOREGROUND",
            "SCHED_SP_TOP_APP",
            "SCHED_SP_SYSTEM",
            "SCHED_SP_RT_APP",
            "CPUSET_SP_DEFAULT",
            "CPUSET_SP_BACKGROUND",
            "CPUSET_SP_FOREGROUND",
            "CPUSET_SP_TOP_APP",
            "CPUSET_SP_SYSTEM",
            "CPUSET_SP_RESTRICTED",
            "BlkIOBackground",
            "BlkIOForeground",
            "ServiceCapacityLow",
        ];
        for name in asked {
            assert!(plain.iter().chain(&aggregate).any(|n| n == name), "{name} is missing");
        }
        for a in profiles["AggregateProfiles"].as_array().expect("a list") {
            for part in a["Profiles"].as_array().expect("a list") {
                let part = part.as_str().expect("a name");
                assert!(plain.iter().any(|n| n == part), "{} is made of {part}, which is not a profile", a["Name"]);
            }
        }
    }

    #[test]
    fn the_partition_is_as_large_as_a_stock_image_ending_in_its_footer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recovery.img");
        let mut whole = vec![0u8; 8192];
        whole[8192 - 64..8192 - 60].copy_from_slice(b"AVBf");
        std::fs::write(&path, &whole).unwrap();
        assert_eq!(partition_size(&path).unwrap(), 8192);
        std::fs::write(&path, &whole[..8000]).unwrap();
        assert!(partition_size(&path).is_err(), "an image cut short of its partition's end");
        std::fs::write(&path, b"AVBf").unwrap();
        assert!(partition_size(&path).is_err(), "too short to end in a footer");
    }

    #[test]
    fn a_file_without_a_last_newline_keeps_its_last_line() {
        let out = without(b"a=1\nro.build.version.release=13", &BUILD_VERSION);
        assert_eq!(out, b"a=1\n");
        let kept = without(b"a=1\nb=2", &BUILD_VERSION);
        assert_eq!(kept, b"a=1\nb=2");
    }

    #[test]
    fn a_second_partition_is_named_where_it_can_only_be_one() {
        for good in ["odm", "vendor_boot", "system-a", "vbmeta_system"] {
            assert!(separate_partition(good).is_ok(), "{good} is a partition name");
        }
        for bad in ["", "recovery", "../recovery", "odm/../../../etc/passwd", "odm.img", "odm ", "ODM\n", "分区"] {
            assert!(separate_partition(bad).is_err(), "{bad:?} is not a partition name");
        }
    }

    /// A configuration naming one program in each of the three trees, and a
    /// firmware file and a library folder beside them.
    const PATHS_CONFIG: &str = "\
[device]
codename = \"x\"

[[service]]
path = \"/vendor/bin/hw/keymint\"
libraries = [\"/system/lib64\", \"/vendor/lib64\"]

[[service]]
path = \"/odm/bin/hw/weaver\"
libraries = [\"/odm/lib64\"]

[wlan]
modules = [\"rfkill\"]
interface = \"wlan0\"
supplicant = \"/vendor/bin/hw/wpa_supplicant\"
service = \"android.hardware.wifi.supplicant.ISupplicant/default\"
probe = \"http://connect.rom.miui.com/generate_204\"
firmware = [{ name = \"peach/WCNSS_qcom_cfg.ini\", path = \"/vendor/etc/wifi/peach/WCNSS_qcom_cfg.ini\" }]
";

    /// A tree holding every `paths` file, made as the partition would be.
    fn tree(root: &Path, paths: &[&str]) -> PathBuf {
        for path in paths {
            let at = root.join(path);
            std::fs::create_dir_all(at.parent().expect("a parent")).unwrap();
            std::fs::write(&at, b"x").unwrap();
        }
        root.to_path_buf()
    }

    fn config_at(dir: &Path) -> PathBuf {
        let at = dir.join("device.toml");
        std::fs::write(&at, PATHS_CONFIG).unwrap();
        at
    }

    #[test]
    fn every_path_a_configuration_names_is_looked_for_in_its_own_partition() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_at(dir.path());
        let vendor = tree(&dir.path().join("vendor"), &[
            "bin/hw/keymint",
            "bin/hw/wpa_supplicant",
            "etc/wifi/peach/WCNSS_qcom_cfg.ini",
        ]);
        std::fs::create_dir_all(vendor.join("lib64")).unwrap();
        let odm = tree(&dir.path().join("odm"), &["bin/hw/weaver"]);
        std::fs::create_dir_all(odm.join("lib64")).unwrap();
        let trees = [("vendor", Some(vendor)), ("odm", Some(odm))];
        check_paths(&config, &trees).expect("every path is there");
    }

    #[test]
    fn a_program_that_is_not_in_its_partition_fails_the_check() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_at(dir.path());
        let vendor = tree(&dir.path().join("vendor"), &["bin/hw/keymint"]);
        std::fs::create_dir_all(vendor.join("lib64")).unwrap();
        let odm = tree(&dir.path().join("odm"), &["bin/hw/weaver"]);
        std::fs::create_dir_all(odm.join("lib64")).unwrap();
        let trees = [("vendor", Some(vendor)), ("odm", Some(odm))];
        // The wpa_supplicant, the firmware file and the system's own library
        // folder are missing, and no system partition was given.
        let error = check_paths(&config, &trees).unwrap_err().to_string();
        assert!(error.contains("are in none of the partitions given"), "{error}");
    }

    #[test]
    fn a_path_whose_partition_was_not_given_is_not_counted_as_checked() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_at(dir.path());
        let vendor = tree(&dir.path().join("vendor"), &["bin/hw/keymint"]);
        std::fs::create_dir_all(vendor.join("lib64")).unwrap();
        let trees = [("vendor", Some(vendor)), ("odm", None)];
        let error = check_paths(&config, &trees).unwrap_err().to_string();
        assert!(error.contains("only half checked") && error.contains("--odm"), "the weaver's partition was not given, so the check cannot pass: {error}");
    }
}
