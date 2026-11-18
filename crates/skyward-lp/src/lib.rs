use anyhow::{anyhow, bail, ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::mem::size_of;
#[cfg(unix)]
use std::os::unix::fs::FileExt;
#[cfg(windows)]
use std::os::windows::fs::FileExt;

#[cfg(unix)]
fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    file.read_exact_at(buf, offset)
}
#[cfg(windows)]
fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    file.seek_read(buf, offset).and_then(|read| {
        if read == buf.len() {
            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "short metadata read",
            ))
        }
    })
}
#[cfg(unix)]
fn write_all_at(file: &File, buf: &[u8], offset: u64) -> std::io::Result<()> {
    file.write_all_at(buf, offset)
}
#[cfg(windows)]
fn write_all_at(file: &File, buf: &[u8], offset: u64) -> std::io::Result<()> {
    file.seek_write(buf, offset).and_then(|written| {
        if written == buf.len() {
            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "short metadata write",
            ))
        }
    })
}
use std::path::Path;
use zerocopy::byteorder::little_endian::{U16, U32, U64};
use zerocopy::{FromBytes, FromZeros, Immutable, IntoBytes, KnownLayout, Unaligned};

/// Sectors are 512 bytes whatever the storage's block size.
pub const SECTOR_SIZE: u64 = 512;

pub const ATTR_READONLY: u32 = 1 << 0;
pub const ATTR_SLOT_SUFFIXED: u32 = 1 << 1;
pub const ATTR_UPDATED: u32 = 1 << 2;
pub const ATTR_DISABLED: u32 = 1 << 3;

pub const HEADER_FLAG_VIRTUAL_AB_DEVICE: u32 = 1 << 0;
pub const HEADER_FLAG_OVERLAYS_ACTIVE: u32 = 1 << 1;

const GEOMETRY_MAGIC: u32 = 0x616c_4467;
const GEOMETRY_SIZE: u64 = 4096;
const HEADER_MAGIC: u32 = 0x414c_5030;
const MAJOR_VERSION: u16 = 10;
const MINOR_VERSION_MAX: u16 = 2;
const VERSION_FOR_UPDATED_ATTR: u16 = 1;
const VERSION_FOR_EXPANDED_HEADER: u16 = 2;
/// Kept free at the start of super so it never looks like a boot sector.
const RESERVED_BYTES: u64 = 4096;
const TARGET_LINEAR: u32 = 0;
const TARGET_ZERO: u32 = 1;
const GROUP_SLOT_SUFFIXED: u32 = 1 << 0;
const BLOCK_DEVICE_SLOT_SUFFIXED: u32 = 1 << 0;
const NAME_LEN: usize = 36;

// On-disk structures, little-endian and packed.

#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Clone)]
#[repr(C)]
struct RawGeometry {
    magic: U32,
    struct_size: U32,
    checksum: [u8; 32],
    metadata_max_size: U32,
    metadata_slot_count: U32,
    logical_block_size: U32,
}

#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Clone)]
#[repr(C)]
struct RawTable {
    /// Relative to the end of the header.
    offset: U32,
    num_entries: U32,
    entry_size: U32,
}

/// The 1.2 header; 1.0 and 1.1 headers stop before `flags`.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Clone)]
#[repr(C)]
struct RawHeader {
    magic: U32,
    major_version: U16,
    minor_version: U16,
    header_size: U32,
    header_checksum: [u8; 32],
    tables_size: U32,
    tables_checksum: [u8; 32],
    partitions: RawTable,
    extents: RawTable,
    groups: RawTable,
    block_devices: RawTable,
    flags: U32,
    reserved: [u8; 124],
}

const HEADER_V1_0_SIZE: usize = 128;

#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawPartition {
    name: [u8; NAME_LEN],
    attributes: U32,
    first_extent_index: U32,
    num_extents: U32,
    group_index: U32,
}

#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawExtent {
    num_sectors: U64,
    target_type: U32,
    target_data: U64,
    target_source: U32,
}

#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawGroup {
    name: [u8; NAME_LEN],
    flags: U32,
    maximum_size: U64,
}

#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawBlockDevice {
    first_logical_sector: U64,
    alignment: U32,
    alignment_offset: U32,
    size: U64,
    partition_name: [u8; NAME_LEN],
    flags: U32,
}

const _: () = {
    assert!(size_of::<RawGeometry>() == 52);
    assert!(size_of::<RawHeader>() == 256);
    assert!(std::mem::offset_of!(RawHeader, flags) == HEADER_V1_0_SIZE);
    assert!(size_of::<RawPartition>() == 52);
    assert!(size_of::<RawExtent>() == 24);
    assert!(size_of::<RawGroup>() == 48);
    assert!(size_of::<RawBlockDevice>() == 64);
};

/// How the metadata copies are laid out at the start of super.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub metadata_max_size: u32,
    pub metadata_slot_count: u32,
    pub logical_block_size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    pub geometry: Geometry,
    pub major_version: u16,
    pub minor_version: u16,
    pub header_flags: u32,
    pub partitions: Vec<Partition>,
    pub groups: Vec<Group>,
    /// The first entry is the device holding the metadata itself.
    pub block_devices: Vec<BlockDevice>,
    /// Problems with the primary copies when a backup copy had to be read.
    pub damaged: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    pub name: String,
    pub attributes: u32,
    /// Index into [`Metadata::groups`].
    pub group: usize,
    pub extents: Vec<Extent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Extent {
    /// `sectors` sectors starting at `sector` on block device `device`.
    Linear {
        sectors: u64,
        device: usize,
        sector: u64,
    },
    Zero {
        sectors: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub name: String,
    /// Bytes; 0 means unlimited.
    pub maximum_size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockDevice {
    pub partition_name: String,
    pub first_logical_sector: u64,
    pub alignment: u32,
    pub alignment_offset: u32,
    pub size: u64,
}

impl Extent {
    pub fn sectors(&self) -> u64 {
        match *self {
            Extent::Linear { sectors, .. } | Extent::Zero { sectors } => sectors,
        }
    }
}

impl Partition {
    pub fn sectors(&self) -> u64 {
        self.extents.iter().map(Extent::sectors).sum()
    }

    pub fn bytes(&self) -> u64 {
        self.sectors() * SECTOR_SIZE
    }
}

impl Metadata {
    pub fn partition(&self, name: &str) -> Option<&Partition> {
        self.partitions.iter().find(|p| p.name == name)
    }
}

/// Read the metadata for `slot` (0 is _a, 1 is _b) from a super partition
/// or an image of its start. A damaged primary geometry or metadata copy is
/// replaced by its backup, and the damage is listed in `damaged`.
pub fn read(super_dev: &Path, slot: u32) -> Result<Metadata> {
    let file = File::open(super_dev).with_context(|| format!("open {}", super_dev.display()))?;
    let at = |offset: u64, len: usize| -> Result<Vec<u8>> {
        let mut buf = vec![0u8; len];
        read_exact_at(&file, &mut buf, offset)
            .with_context(|| format!("read {len} bytes at {offset} in {}", super_dev.display()))?;
        Ok(buf)
    };

    let mut damaged = Vec::new();
    let geometry = match Geometry::parse(&at(RESERVED_BYTES, GEOMETRY_SIZE as usize)?) {
        Ok(g) => g,
        Err(e) => {
            damaged.push(format!("primary geometry: {e:#}"));
            Geometry::parse(&at(RESERVED_BYTES + GEOMETRY_SIZE, GEOMETRY_SIZE as usize)?)
                .with_context(|| format!("both geometry copies are damaged; primary: {e:#}"))?
        }
    };
    ensure!(
        slot < geometry.metadata_slot_count,
        "metadata slot {slot} does not exist; super keeps {}",
        geometry.metadata_slot_count
    );

    let size = geometry.metadata_max_size as usize;
    let mut metadata = match parse_metadata(
        &geometry,
        &at(geometry.primary_offset(slot), size)?,
        Some(slot),
    ) {
        Ok(m) => m,
        Err(e) => {
            damaged.push(format!("primary metadata for slot {slot}: {e:#}"));
            parse_metadata(
                &geometry,
                &at(geometry.backup_offset(slot), size)?,
                Some(slot),
            )
            .with_context(|| {
                format!("both metadata copies for slot {slot} are damaged; primary: {e:#}")
            })?
        }
    };
    metadata.damaged = damaged;
    Ok(metadata)
}

/// Read metadata kept in a file of its own, as libfiemap keeps where its
/// images lie on the data partition: the geometry, then a single copy of
/// the metadata, its names taken as they are.
pub fn read_image(path: &Path) -> Result<Metadata> {
    let data = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let geometry = data
        .get(..GEOMETRY_SIZE as usize)
        .with_context(|| format!("{} ends inside its geometry", path.display()))?;
    let geometry = Geometry::parse(geometry)
        .with_context(|| format!("read the geometry of {}", path.display()))?;
    parse_metadata(&geometry, &data[GEOMETRY_SIZE as usize..], None)
        .with_context(|| format!("read the metadata of {}", path.display()))
}

impl Geometry {
    fn parse(block: &[u8]) -> Result<Geometry> {
        let (raw, _) =
            RawGeometry::read_from_prefix(block).map_err(|_| anyhow!("short geometry block"))?;
        ensure!(
            raw.magic.get() == GEOMETRY_MAGIC,
            "bad magic {:#x}",
            raw.magic.get()
        );
        let size = raw.struct_size.get() as usize;
        ensure!(size == size_of::<RawGeometry>(), "struct size {size}");
        let mut unsummed = raw.clone();
        unsummed.checksum = [0; 32];
        ensure!(
            sha256(&unsummed.as_bytes()[..size]) == raw.checksum,
            "bad checksum"
        );
        let g = Geometry {
            metadata_max_size: raw.metadata_max_size.get(),
            metadata_slot_count: raw.metadata_slot_count.get(),
            logical_block_size: raw.logical_block_size.get(),
        };
        ensure!(g.metadata_slot_count != 0, "no metadata slots");
        ensure!(
            u64::from(g.metadata_max_size) % SECTOR_SIZE == 0,
            "metadata size {} is not a whole number of sectors",
            g.metadata_max_size
        );
        Ok(g)
    }

    fn primary_offset(&self, slot: u32) -> u64 {
        RESERVED_BYTES + GEOMETRY_SIZE * 2 + u64::from(self.metadata_max_size) * u64::from(slot)
    }

    fn backup_offset(&self, slot: u32) -> u64 {
        self.primary_offset(slot)
            + u64::from(self.metadata_max_size) * u64::from(self.metadata_slot_count)
    }

    /// Bytes from the start of super to the end of the last metadata copy.
    fn metadata_region(&self) -> u64 {
        RESERVED_BYTES
            + (GEOMETRY_SIZE
                + u64::from(self.metadata_max_size) * u64::from(self.metadata_slot_count))
                * 2
    }
}

fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

impl RawTable {
    fn fits(&self, tables_size: u32) -> bool {
        let offset = u64::from(self.offset.get());
        let size = u64::from(self.num_entries.get()) * u64::from(self.entry_size.get());
        offset <= u64::from(tables_size) && size <= u64::from(tables_size) - offset
    }

    /// Each entry's first `size_of::<T>()` bytes; `fits` has checked the bounds.
    fn entries<T: FromBytes>(&self, tables: &[u8]) -> Vec<T> {
        let (offset, entry) = (self.offset.get() as usize, self.entry_size.get() as usize);
        (0..self.num_entries.get() as usize)
            .map(|i| {
                let start = offset + i * entry;
                T::read_from_bytes(&tables[start..start + size_of::<T>()])
                    .expect("slice has the entry's size")
            })
            .collect()
    }
}

/// A NUL-padded name; a name filling the whole field has no terminator.
fn name(field: &[u8; NAME_LEN]) -> Result<String> {
    let end = field.iter().position(|&b| b == 0).unwrap_or(NAME_LEN);
    String::from_utf8(field[..end].to_vec())
        .map_err(|_| anyhow!("name {:?} is not text", &field[..end]))
}

/// Names flagged as slot-suffixed (retrofit devices) get the suffix of the
/// slot read, where one is.
fn suffixed(name: String, flagged: bool, slot: Option<u32>) -> Result<String> {
    let Some(slot) = slot.filter(|_| flagged) else {
        return Ok(name);
    };
    let suffix = match slot {
        0 => "_a",
        1 => "_b",
        _ => bail!("{name} needs a slot suffix, and slot {slot} has none"),
    };
    let full = name + suffix;
    ensure!(
        full.len() <= NAME_LEN,
        "{full} is longer than {NAME_LEN} bytes"
    );
    Ok(full)
}

fn parse_metadata(geometry: &Geometry, buf: &[u8], slot: Option<u32>) -> Result<Metadata> {
    ensure!(buf.len() >= size_of::<RawHeader>(), "short metadata copy");
    let mut header = RawHeader::new_zeroed();
    header.as_mut_bytes()[..HEADER_V1_0_SIZE].copy_from_slice(&buf[..HEADER_V1_0_SIZE]);
    ensure!(
        header.magic.get() == HEADER_MAGIC,
        "bad magic {:#x}",
        header.magic.get()
    );
    let (major, minor) = (header.major_version.get(), header.minor_version.get());
    ensure!(
        major == MAJOR_VERSION && minor <= MINOR_VERSION_MAX,
        "unsupported version {major}.{minor}"
    );
    let expected = if minor < VERSION_FOR_EXPANDED_HEADER {
        HEADER_V1_0_SIZE
    } else {
        size_of::<RawHeader>()
    };
    let header_size = header.header_size.get() as usize;
    ensure!(
        header_size == expected,
        "header size {header_size} for version {major}.{minor}"
    );
    header.as_mut_bytes()[HEADER_V1_0_SIZE..header_size]
        .copy_from_slice(&buf[HEADER_V1_0_SIZE..header_size]);
    let mut unsummed = header.clone();
    unsummed.header_checksum = [0; 32];
    ensure!(
        sha256(&unsummed.as_bytes()[..header_size]) == header.header_checksum,
        "bad header checksum"
    );

    let tables_size = header.tables_size.get();
    for (what, table) in [
        ("partition", &header.partitions),
        ("extent", &header.extents),
        ("group", &header.groups),
        ("block device", &header.block_devices),
    ] {
        ensure!(table.fits(tables_size), "{what} table runs past the tables");
    }
    for (what, table, size) in [
        ("partition", &header.partitions, size_of::<RawPartition>()),
        ("extent", &header.extents, size_of::<RawExtent>()),
        ("group", &header.groups, size_of::<RawGroup>()),
    ] {
        ensure!(
            table.entry_size.get() as usize == size,
            "{what} entries take {} bytes",
            table.entry_size.get()
        );
    }
    ensure!(
        header.block_devices.entry_size.get() as usize >= size_of::<RawBlockDevice>(),
        "block device entries take {} bytes",
        header.block_devices.entry_size.get()
    );
    ensure!(
        tables_size <= geometry.metadata_max_size,
        "tables take {tables_size} bytes"
    );
    let tables = buf
        .get(header_size..header_size + tables_size as usize)
        .context("tables run past the metadata copy")?;
    ensure!(
        sha256(tables) == header.tables_checksum,
        "bad tables checksum"
    );

    let block_devices = header
        .block_devices
        .entries::<RawBlockDevice>(tables)
        .iter()
        .map(|d| {
            Ok(BlockDevice {
                partition_name: suffixed(
                    name(&d.partition_name)?,
                    d.flags.get() & BLOCK_DEVICE_SLOT_SUFFIXED != 0,
                    slot,
                )?,
                first_logical_sector: d.first_logical_sector.get(),
                alignment: d.alignment.get(),
                alignment_offset: d.alignment_offset.get(),
                size: d.size.get(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let super_device = block_devices.first().context("no block devices")?;
    ensure!(
        geometry.metadata_region() <= super_device.first_logical_sector * SECTOR_SIZE,
        "metadata overlaps the partitions"
    );

    let extents = header
        .extents
        .entries::<RawExtent>(tables)
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let sectors = e.num_sectors.get();
            Ok(match e.target_type.get() {
                TARGET_LINEAR => {
                    let device = e.target_source.get() as usize;
                    ensure!(
                        device < block_devices.len(),
                        "extent {i} is on block device {device}"
                    );
                    Extent::Linear {
                        sectors,
                        device,
                        sector: e.target_data.get(),
                    }
                }
                TARGET_ZERO => Extent::Zero { sectors },
                t => bail!("extent {i} has target type {t}"),
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let groups = header
        .groups
        .entries::<RawGroup>(tables)
        .iter()
        .map(|g| {
            Ok(Group {
                name: suffixed(
                    name(&g.name)?,
                    g.flags.get() & GROUP_SLOT_SUFFIXED != 0,
                    slot,
                )?,
                maximum_size: g.maximum_size.get(),
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let valid = if minor >= VERSION_FOR_UPDATED_ATTR {
        ATTR_READONLY | ATTR_SLOT_SUFFIXED | ATTR_UPDATED | ATTR_DISABLED
    } else {
        ATTR_READONLY | ATTR_SLOT_SUFFIXED
    };
    let partitions = header
        .partitions
        .entries::<RawPartition>(tables)
        .iter()
        .map(|p| {
            let name = name(&p.name)?;
            let attributes = p.attributes.get();
            ensure!(
                attributes & !valid == 0,
                "{name} has attributes {attributes:#x}"
            );
            let (first, count) = (
                p.first_extent_index.get() as usize,
                p.num_extents.get() as usize,
            );
            ensure!(
                first + count <= extents.len(),
                "{name} owns extents {first}..{}",
                first + count
            );
            let group = p.group_index.get() as usize;
            ensure!(group < groups.len(), "{name} is in group {group}");
            Ok(Partition {
                name: suffixed(name, attributes & ATTR_SLOT_SUFFIXED != 0, slot)?,
                attributes: attributes & !ATTR_SLOT_SUFFIXED,
                group,
                extents: extents[first..first + count].to_vec(),
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(Metadata {
        geometry: *geometry,
        major_version: major,
        minor_version: minor,
        header_flags: header.flags.get(),
        partitions,
        groups,
        block_devices,
        damaged: Vec::new(),
    })
}

fn slot_suffix(name: &str) -> Option<&str> {
    name.strip_suffix("_a").or_else(|| name.strip_suffix("_b"))
}

/// Encode a geometry block, including its checksum and zero padding.
pub fn encode_geometry(geometry: &Geometry) -> Result<Vec<u8>> {
    ensure!(geometry.metadata_slot_count != 0, "no metadata slots");
    ensure!(
        u64::from(geometry.metadata_max_size) % SECTOR_SIZE == 0,
        "metadata size {} is not a whole number of sectors",
        geometry.metadata_max_size
    );
    let mut raw = RawGeometry {
        magic: GEOMETRY_MAGIC.into(),
        struct_size: (size_of::<RawGeometry>() as u32).into(),
        checksum: [0; 32],
        metadata_max_size: geometry.metadata_max_size.into(),
        metadata_slot_count: geometry.metadata_slot_count.into(),
        logical_block_size: geometry.logical_block_size.into(),
    };
    raw.checksum = sha256(raw.as_bytes());
    let mut block = vec![0; GEOMETRY_SIZE as usize];
    block[..size_of::<RawGeometry>()].copy_from_slice(raw.as_bytes());
    Ok(block)
}

fn raw_name(name: &str, what: &str) -> Result<[u8; NAME_LEN]> {
    ensure!(!name.is_empty(), "{what} name is empty");
    ensure!(
        name.len() <= NAME_LEN,
        "{what} name {name} is longer than {NAME_LEN} bytes"
    );
    ensure!(
        !name.as_bytes().contains(&0),
        "{what} name {name:?} contains NUL"
    );
    let mut raw = [0; NAME_LEN];
    raw[..name.len()].copy_from_slice(name.as_bytes());
    Ok(raw)
}

fn raw_table(offset: usize, count: usize, entry_size: usize) -> Result<RawTable> {
    Ok(RawTable {
        offset: u32::try_from(offset)
            .context("table offset does not fit on disk")?
            .into(),
        num_entries: u32::try_from(count)
            .context("table entry count does not fit on disk")?
            .into(),
        entry_size: u32::try_from(entry_size)
            .context("table entry size does not fit on disk")?
            .into(),
    })
}

/// Serialize all metadata tables using the source metadata's version.
/// Retrofitted A/B names are reconstructed in their slot-independent form.
///
/// The returned buffer contains just the header and tables, not slot padding.
/// Callers must not modify a metadata slot without also checking that this
/// buffer fits [`Geometry::metadata_max_size`].
pub fn encode_metadata(metadata: &Metadata) -> Result<Vec<u8>> {
    validate_metadata(metadata)?;
    ensure!(
        metadata.major_version == MAJOR_VERSION,
        "unsupported major version {}",
        metadata.major_version
    );
    ensure!(
        metadata.minor_version <= MINOR_VERSION_MAX,
        "unsupported minor version {}",
        metadata.minor_version
    );
    let minor = metadata.minor_version;
    let header_size = if minor < VERSION_FOR_EXPANDED_HEADER { HEADER_V1_0_SIZE } else { size_of::<RawHeader>() };
    let mut raw_partitions = Vec::new();
    let mut raw_extents = Vec::new();
    for partition in &metadata.partitions {
        let first_extent_index = raw_extents.len();
        for extent in &partition.extents {
            let (target_type, target_data, target_source) = match *extent {
                Extent::Linear { device, sector, .. } => (TARGET_LINEAR, sector, device as u32),
                Extent::Zero { .. } => (TARGET_ZERO, 0, 0),
            };
            raw_extents.push(RawExtent {
                num_sectors: extent.sectors().into(),
                target_type: target_type.into(),
                target_data: target_data.into(),
                target_source: target_source.into(),
            });
        }
        raw_partitions.push(RawPartition {
            name: raw_name(slot_suffix(&partition.name).map_or(partition.name.as_str(), |base| base), "partition")?,
            attributes: (partition.attributes | if slot_suffix(&partition.name).is_some() { ATTR_SLOT_SUFFIXED } else { 0 }).into(),
            first_extent_index: u32::try_from(first_extent_index)
                .context("too many extents")?
                .into(),
            num_extents: u32::try_from(partition.extents.len())
                .context("too many extents")?
                .into(),
            group_index: u32::try_from(partition.group)
                .context("group index does not fit on disk")?
                .into(),
        });
    }
    let raw_groups = metadata
        .groups
        .iter()
        .map(|group| {
            Ok(RawGroup {
                name: raw_name(slot_suffix(&group.name).map_or(group.name.as_str(), |base| base), "group")?,
                flags: if slot_suffix(&group.name).is_some() { GROUP_SLOT_SUFFIXED } else { 0 }.into(),
                maximum_size: group.maximum_size.into(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let raw_block_devices = metadata
        .block_devices
        .iter()
        .map(|device| {
            Ok(RawBlockDevice {
                first_logical_sector: device.first_logical_sector.into(),
                alignment: device.alignment.into(),
                alignment_offset: device.alignment_offset.into(),
                size: device.size.into(),
                partition_name: raw_name(slot_suffix(&device.partition_name).map_or(device.partition_name.as_str(), |base| base), "block device")?,
                flags: if slot_suffix(&device.partition_name).is_some() { BLOCK_DEVICE_SLOT_SUFFIXED } else { 0 }.into(),
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let mut tables = Vec::new();
    macro_rules! table {
        ($entries:expr, $type:ty) => {{
            let table = raw_table(tables.len(), $entries.len(), size_of::<$type>())?;
            for entry in &$entries {
                tables.extend_from_slice(entry.as_bytes());
            }
            table
        }};
    }
    let partitions = table!(raw_partitions, RawPartition);
    let extents = table!(raw_extents, RawExtent);
    let groups = table!(raw_groups, RawGroup);
    let block_devices = table!(raw_block_devices, RawBlockDevice);
    let tables_size = u32::try_from(tables.len()).context("metadata tables are too large")?;
    let total = header_size
        .checked_add(tables.len())
        .context("metadata size overflow")?;
    ensure!(
        total <= metadata.geometry.metadata_max_size as usize,
        "encoded metadata takes {total} bytes, but the slot holds {}",
        metadata.geometry.metadata_max_size
    );
    let mut header = RawHeader {
        magic: HEADER_MAGIC.into(),
        major_version: MAJOR_VERSION.into(),
        minor_version: minor.into(),
        header_size: (header_size as u32).into(),
        header_checksum: [0; 32],
        tables_size: tables_size.into(),
        tables_checksum: sha256(&tables),
        partitions,
        extents,
        groups,
        block_devices,
        flags: metadata.header_flags.into(),
        reserved: [0; 124],
    };
    header.header_checksum = sha256(&header.as_bytes()[..header_size]);
    let mut encoded = header.as_bytes()[..header_size].to_vec();
    encoded.extend_from_slice(&tables);
    Ok(encoded)
}

fn device_range(device: &BlockDevice) -> Result<std::ops::Range<u64>> {
    let sectors = device.size / SECTOR_SIZE;
    ensure!(
        device.size.is_multiple_of(SECTOR_SIZE),
        "{} is not a whole number of sectors",
        device.partition_name
    );
    let end = device
        .first_logical_sector
        .checked_add(sectors)
        .context("block device size overflow")?;
    Ok(device.first_logical_sector..end)
}

/// Check references and physical ranges shared by encoding and allocation.
fn validate_metadata(metadata: &Metadata) -> Result<()> {
    let super_device = metadata.block_devices.first().context("no block devices")?;
    ensure!(
        metadata.geometry.metadata_region()
            <= super_device
                .first_logical_sector
                .saturating_mul(SECTOR_SIZE),
        "metadata overlaps the partitions"
    );
    let ranges = metadata
        .block_devices
        .iter()
        .map(device_range)
        .collect::<Result<Vec<_>>>()?;
    let mut used = Vec::new();
    for partition in &metadata.partitions {
        ensure!(
            partition.group < metadata.groups.len(),
            "{} is in group {}",
            partition.name,
            partition.group
        );
        for extent in &partition.extents {
            ensure!(
                extent.sectors() != 0,
                "{} has an empty extent",
                partition.name
            );
            if let Extent::Linear {
                device,
                sector,
                sectors,
            } = *extent
            {
                let range = ranges.get(device).with_context(|| {
                    format!("{} uses missing block device {device}", partition.name)
                })?;
                let end = sector
                    .checked_add(sectors)
                    .context("extent size overflow")?;
                ensure!(
                    sector >= range.start && end <= range.end,
                    "{} has an extent outside block device {device}",
                    partition.name
                );
                used.push((device, sector..end));
            }
        }
    }
    for (i, (device, range)) in used.iter().enumerate() {
        for (other_device, other) in &used[..i] {
            ensure!(
                device != other_device || range.end <= other.start || other.end <= range.start,
                "linear extents overlap"
            );
        }
    }
    Ok(())
}

fn aligned_sector(device: &BlockDevice, sector: u64) -> Result<u64> {
    if device.alignment == 0 {
        return Ok(sector);
    }
    let alignment = u64::from(device.alignment);
    ensure!(
        alignment % SECTOR_SIZE == 0,
        "{} has non-sector alignment",
        device.partition_name
    );
    let offset = u64::from(device.alignment_offset);
    let bytes = sector
        .checked_mul(SECTOR_SIZE)
        .context("alignment calculation overflow")?;
    let since_offset = bytes.saturating_sub(offset);
    let misalignment = since_offset % alignment;
    if misalignment == 0 {
        Ok(sector)
    } else {
        let advance = alignment - misalignment;
        ensure!(
            advance % SECTOR_SIZE == 0,
            "alignment cannot be reached in whole sectors"
        );
        sector
            .checked_add(advance / SECTOR_SIZE)
            .context("aligned sector overflow")
    }
}

/// Grow a partition to exactly `new_size` bytes without changing an existing
/// extent. Space is allocated from free ranges in ascending sector order; a
/// large enough tail is used, otherwise several gaps contribute new extents.
pub fn grow(metadata: &Metadata, name: &str, new_size: u64) -> Result<Metadata> {
    grow_avoiding(metadata, name, new_size, &[])
}

/// [`grow`], with `taken` — sectors on a block device, by its index — kept
/// clear as well.
///
/// Super holds both slots' partitions, and each slot's metadata names only
/// its own: a slot grown on its own would otherwise be free to lay a new
/// extent over the other slot's partitions, which are there all the same.
pub fn grow_avoiding(
    metadata: &Metadata,
    name: &str,
    new_size: u64,
    taken: &[(usize, std::ops::Range<u64>)],
) -> Result<Metadata> {
    let partition = metadata
        .partition(name)
        .with_context(|| format!("no partition named {name}"))?;
    let old_size = partition.bytes();
    ensure!(
        new_size > old_size,
        "new size {new_size} is not greater than {old_size}"
    );
    let growth = new_size - old_size;
    ensure!(
        growth.is_multiple_of(SECTOR_SIZE),
        "growth is not a whole number of sectors"
    );
    ensure!(!partition.extents.is_empty(), "{name} has no extents");
    let group = metadata
        .groups
        .get(partition.group)
        .with_context(|| format!("invalid group for {name}"))?;
    if group.maximum_size != 0 {
        let group_size = metadata
            .partitions
            .iter()
            .filter(|p| p.group == partition.group)
            .try_fold(growth, |sum, p| sum.checked_add(p.bytes()))
            .context("group size overflow")?;
        ensure!(
            group_size <= group.maximum_size,
            "group {} would take {group_size} bytes, over its {} byte limit",
            group.name,
            group.maximum_size
        );
    }

    let mut result = metadata.clone();
    result.damaged.clear();
    validate_metadata(&result)?;
    let mut used = Vec::<(usize, std::ops::Range<u64>)>::new();
    for partition in &result.partitions {
        for extent in &partition.extents {
            if let Extent::Linear {
                device,
                sector,
                sectors,
            } = *extent
            {
                used.push((device, sector..sector + sectors));
            }
        }
    }
    let metadata_sectors = metadata.geometry.metadata_region().div_ceil(SECTOR_SIZE);
    used.push((0, 0..metadata_sectors));
    // The sectors another slot of super holds: on a device this metadata does
    // not list, an index out of range only means there is nowhere to put it.
    for (device, range) in taken {
        if *device < result.block_devices.len() {
            used.push((*device, range.clone()));
        }
    }
    used.sort_by_key(|(_, range)| range.start);
    let mut remaining = growth / SECTOR_SIZE;
    let mut additions = Vec::new();
    for (index, device) in result.block_devices.iter().enumerate() {
        let range = device_range(device)?;
        let last = partition
            .extents
            .iter()
            .rev()
            .find_map(|extent| match *extent {
                Extent::Linear {
                    device,
                    sector,
                    sectors,
                } if device == index => Some(sector + sectors),
                _ => None,
            });
        let mut free_start = last.unwrap_or(range.start);
        for (_, occupied) in used.iter().filter(|(d, _)| *d == index) {
            if occupied.end <= free_start {
                continue;
            }
            if occupied.start > free_start {
                let start = aligned_sector(device, free_start)?;
                let available = occupied.start.saturating_sub(start);
                let take = available.min(remaining);
                if take != 0 {
                    additions.push(Extent::Linear {
                        sectors: take,
                        device: index,
                        sector: start,
                    });
                    remaining -= take;
                }
            }
            free_start = free_start.max(occupied.end);
            if remaining == 0 {
                break;
            }
        }
        if remaining != 0 && free_start < range.end {
            let start = aligned_sector(device, free_start)?;
            let take = range.end.saturating_sub(start).min(remaining);
            if take != 0 {
                additions.push(Extent::Linear {
                    sectors: take,
                    device: index,
                    sector: start,
                });
                remaining -= take;
            }
        }
        if remaining == 0 {
            break;
        }
    }
    ensure!(
        remaining == 0,
        "not enough aligned free space: {remaining} sectors still needed"
    );
    result
        .partitions
        .iter_mut()
        .find(|p| p.name == name)
        .unwrap()
        .extents
        .extend(additions);
    encode_metadata(&result)?;
    Ok(result)
}

/// Replace both copies of one metadata slot, preserving the other slots and
/// geometry. Both copies are synced and read back before this returns.
pub fn commit(super_dev: &Path, slot: u32, metadata: &Metadata) -> Result<()> {
    let encoded = encode_metadata(metadata)?;
    ensure!(
        slot < metadata.geometry.metadata_slot_count,
        "metadata slot {slot} does not exist"
    );
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(super_dev)
        .with_context(|| format!("open {}", super_dev.display()))?;
    let slot_size = u64::from(metadata.geometry.metadata_max_size);
    for offset in [
        metadata.geometry.primary_offset(slot),
        metadata.geometry.backup_offset(slot),
    ] {
        write_all_at(&file, &encoded, offset)
            .with_context(|| format!("write metadata at {offset}"))?;
        let padding = slot_size - encoded.len() as u64;
        if padding != 0 {
            // A single zeroed tail also removes bytes left by a larger table.
            let zeros = vec![0; padding as usize];
            write_all_at(&file, &zeros, offset + encoded.len() as u64)
                .context("zero unused metadata bytes")?;
        }
        file.sync_data().context("sync metadata copy")?;
        let mut read_back = vec![0; encoded.len()];
        read_exact_at(&file, &mut read_back, offset).context("read back metadata copy")?;
        ensure!(
            read_back == encoded,
            "metadata copy at {offset} failed read-back verification"
        );
    }
    file.sync_all().context("sync committed metadata")?;
    Ok(())
}

/// The sectors a slot's metadata gives to its partitions, by the index of the
/// block device they lie on.
fn linear_extents(metadata: &Metadata) -> Vec<(usize, std::ops::Range<u64>)> {
    let mut found = Vec::new();
    for partition in &metadata.partitions {
        for extent in &partition.extents {
            if let Extent::Linear {
                device,
                sector,
                sectors,
            } = *extent
            {
                found.push((device, sector..sector + sectors));
            }
        }
    }
    found
}

/// Grow one partition in each of several slots of super, planned together.
///
/// Super holds every slot's partitions in the same block devices, while each
/// metadata slot names only its own. A slot grown on its own would therefore
/// be free to lay a new extent over another slot's partition. Each request is
/// planned against every other slot's partitions and against the plans before
/// it, and the plans are checked against each other before any of them is
/// written; `requests` is one `(metadata, partition, new size)` per slot.
pub fn grow_across(requests: &[(Metadata, String, u64)]) -> Result<Vec<Metadata>> {
    let mut planned: Vec<Metadata> = Vec::with_capacity(requests.len());
    for (index, (metadata, name, new_size)) in requests.iter().enumerate() {
        let mut taken: Vec<(usize, std::ops::Range<u64>)> = Vec::new();
        for (other, (their_metadata, _, _)) in requests.iter().enumerate() {
            if other != index {
                taken.extend(linear_extents(their_metadata));
            }
        }
        for done in &planned {
            taken.extend(linear_extents(done));
        }
        let current = metadata
            .partition(name)
            .with_context(|| format!("no partition named {name}"))?
            .bytes();
        let grown = match *new_size > current {
            true => grow_avoiding(metadata, name, *new_size, &taken)?,
            false => metadata.clone(),
        };
        planned.push(grown);
    }
    let mut occupied: Vec<(usize, std::ops::Range<u64>)> = Vec::new();
    for metadata in &planned {
        for (device, range) in linear_extents(metadata) {
            ensure!(
                !occupied
                    .iter()
                    .any(|(d, r)| *d == device && range.start < r.end && r.start < range.end),
                "the plans of two slots would use the same sectors"
            );
            occupied.push((device, range));
        }
    }
    Ok(planned)
}
