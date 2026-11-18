use skyward_lp::{
    commit, encode_geometry, encode_metadata, grow, grow_across, grow_avoiding, read, BlockDevice, Extent,
    Geometry, Group, Metadata, Partition, SECTOR_SIZE,
};
use std::path::PathBuf;

fn metadata() -> Metadata {
    Metadata {
        geometry: Geometry {
            metadata_max_size: 4096,
            metadata_slot_count: 2,
            logical_block_size: 4096,
        },
        major_version: 10,
        minor_version: 2,
        header_flags: 0,
        partitions: vec![Partition {
            name: "system".into(),
            attributes: 0,
            group: 0,
            extents: vec![Extent::Linear {
                sectors: 8,
                device: 0,
                sector: 160,
            }],
        }],
        groups: vec![Group {
            name: "dynamic".into(),
            maximum_size: 128 * SECTOR_SIZE,
        }],
        block_devices: vec![BlockDevice {
            partition_name: "super".into(),
            first_logical_sector: 128,
            alignment: 4096,
            alignment_offset: 0,
            size: 512 * SECTOR_SIZE,
        }],
        damaged: vec!["not a disk property".into()],
    }
}

fn scratch(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("skyward-lp-resize-{name}-{}", std::process::id()))
}

#[test]
fn encoding_round_trips_every_table() {
    let mut md = metadata();
    md.partitions.push(Partition {
        name: "empty".into(),
        attributes: 5,
        group: 0,
        extents: vec![],
    });
    md.partitions[0].extents.push(Extent::Zero { sectors: 2 });
    let path = scratch("roundtrip");
    let mut image = encode_geometry(&md.geometry).unwrap();
    image.extend_from_slice(&encode_metadata(&md).unwrap());
    std::fs::write(&path, &image).unwrap();
    // read_image takes geometry then metadata; use a super image for slot names.
    let decoded = skyward_lp::read_image(&path).unwrap();
    std::fs::remove_file(path).unwrap();
    let mut expected = md;
    expected.damaged.clear();
    assert_eq!(decoded, expected);
}

#[test]
fn old_versions_and_retrofit_suffixes_survive_encoding() {
    for minor in 0..=2 {
        let mut md = metadata();
        md.minor_version = minor;
        md.damaged.clear();
        md.partitions[0].name = "system_a".into();
        md.groups[0].name = "dynamic_a".into();
        md.block_devices[0].partition_name = "super_a".into();
        let encoded = encode_metadata(&md).unwrap();
        let header_size = if minor < 2 { 128 } else { 256 };
        assert_eq!(u32::from_le_bytes(encoded[8..12].try_into().unwrap()), header_size);
        let path = scratch(&format!("retrofit-{minor}"));
        let geometry = encode_geometry(&md.geometry).unwrap();
        let mut image = vec![0; 4096];
        image.extend_from_slice(&geometry);
        image.extend_from_slice(&geometry);
        image.extend_from_slice(&encoded);
        image.resize(12 * 1024 + 4 * md.geometry.metadata_max_size as usize, 0);
        std::fs::write(&path, image).unwrap();
        let decoded = read(&path, 0).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(decoded, md);
    }
}

#[test]
fn tail_growth_preserves_existing_extents() {
    let md = metadata();
    let grown = grow(&md, "system", 24 * SECTOR_SIZE).unwrap();
    assert_eq!(md.partition("system").unwrap().extents.len(), 1);
    assert_eq!(
        grown.partition("system").unwrap().extents,
        [
            Extent::Linear {
                sectors: 8,
                device: 0,
                sector: 160
            },
            Extent::Linear {
                sectors: 16,
                device: 0,
                sector: 168
            },
        ]
    );
    assert!(grown.damaged.is_empty());
}

#[test]
fn growth_keeps_clear_what_another_slot_holds() {
    let md = metadata();
    // Nothing else holds anything: the growth follows the partition.
    let grown = grow_avoiding(&md, "system", 24 * SECTOR_SIZE, &[]).unwrap();
    assert_eq!(
        grown.partition("system").unwrap().extents[1],
        Extent::Linear {
            sectors: 16,
            device: 0,
            sector: 168
        }
    );
    // The sectors after it belong to the other slot of super, which this
    // slot's metadata does not name at all. The growth goes past them.
    let taken = [(0usize, 168u64..184u64)];
    let grown = grow_avoiding(&md, "system", 24 * SECTOR_SIZE, &taken).unwrap();
    assert_eq!(
        grown.partition("system").unwrap().extents[1],
        Extent::Linear {
            sectors: 16,
            device: 0,
            sector: 184
        }
    );
    // A sector on a block device this metadata does not list is nowhere it
    // could put anything, and is not a reason to refuse.
    let elsewhere = [(9usize, 0u64..8u64)];
    assert!(grow_avoiding(&md, "system", 24 * SECTOR_SIZE, &elsewhere).is_ok());
}

/// A slot of super holding one partition, at `sector`, of `sectors` sectors.
fn slot(name: &str, sector: u64, sectors: u64) -> Metadata {
    let mut md = metadata();
    md.damaged.clear();
    md.partitions = vec![Partition {
        name: name.into(),
        attributes: 0,
        group: 0,
        extents: vec![Extent::Linear {
            sectors,
            device: 0,
            sector,
        }],
    }];
    md.groups[0].maximum_size = 0;
    md
}

/// Every sector either plan gives to a partition.
fn sectors(md: &Metadata) -> Vec<(usize, std::ops::Range<u64>)> {
    let mut found = Vec::new();
    for part in &md.partitions {
        for extent in &part.extents {
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

#[test]
fn two_slots_grow_without_taking_each_others_partitions() {
    // The two slots' partitions sit next to each other, as super holds them.
    let (a, b) = (slot("system_a", 160, 8), slot("system_b", 168, 8));
    let requests = [
        (a, "system_a".to_string(), 24 * SECTOR_SIZE),
        (b, "system_b".to_string(), 24 * SECTOR_SIZE),
    ];
    let planned = grow_across(&requests).unwrap();
    assert_eq!(planned[0].partition("system_a").unwrap().bytes(), 24 * SECTOR_SIZE);
    assert_eq!(planned[1].partition("system_b").unwrap().bytes(), 24 * SECTOR_SIZE);
    // The first slot's growth had to go past the second slot's partition.
    assert_eq!(
        sectors(&planned[0]),
        [
            (0, 160..168),
            (0, 176..192),
        ],
        "past the other slot's partition, not over it"
    );
    // And the second slot's growth had to go past the first one's new extent.
    assert_eq!(
        sectors(&planned[1]),
        [(0, 168..176), (0, 192..208)],
        "past what the first plan took, not over it"
    );
    // Nothing is given to two partitions at once.
    let all: Vec<_> = planned.iter().flat_map(sectors).collect();
    for (index, (device, range)) in all.iter().enumerate() {
        for (other_device, other) in &all[..index] {
            assert!(
                device != other_device || range.end <= other.start || other.end <= range.start,
                "{device}:{range:?} overlaps {other_device}:{other:?}"
            );
        }
    }
}

#[test]
fn a_slot_with_room_already_is_left_exactly_as_it_was() {
    let (a, b) = (slot("system_a", 160, 8), slot("system_b", 168, 8));
    let requests = [
        (a.clone(), "system_a".to_string(), 8 * SECTOR_SIZE),
        (b.clone(), "system_b".to_string(), 24 * SECTOR_SIZE),
    ];
    let planned = grow_across(&requests).unwrap();
    assert_eq!(planned[0], a, "nothing to plan means nothing changed");
    assert_eq!(planned[1].partition("system_b").unwrap().bytes(), 24 * SECTOR_SIZE);
}

#[test]
fn a_plan_that_would_not_fit_is_refused() {
    // Both slots would have to grow into the same last few sectors.
    let mut small = slot("system_a", 160, 8);
    small.block_devices[0].first_logical_sector = 128;
    small.block_devices[0].size = 48 * SECTOR_SIZE; // 128..176
    let b = small.clone();
    let requests = [
        (small, "system_a".to_string(), 40 * SECTOR_SIZE),
        (b, "system_b".to_string(), 40 * SECTOR_SIZE),
    ];
    assert!(grow_across(&requests).is_err(), "there is room for one of them, not both");
}

#[test]
fn growth_never_takes_a_sector_that_was_not_its_own() {
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut stream = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for round in 0..40 {
        let start = 128u64;
        let length = 200 + stream() % 600;
        let end = start + length;
        let mut md = metadata();
        md.damaged.clear();
        // A group with no limit, so that only the device's own room decides.
        md.groups[0].maximum_size = 0;
        md.block_devices[0].alignment = 0;
        md.block_devices[0].first_logical_sector = start;
        md.block_devices[0].size = length * SECTOR_SIZE;
        let at = start + 40 + stream() % (length - 80);
        md.partitions = vec![Partition {
            name: "system".into(),
            attributes: 0,
            group: 0,
            extents: vec![Extent::Linear {
                sectors: 8,
                device: 0,
                sector: at,
            }],
        }];
        // What another slot of super holds, which is not this one's to take.
        let mut taken: Vec<(usize, std::ops::Range<u64>)> = Vec::new();
        for _ in 0..stream() % 3 {
            let from = start + stream() % length;
            let to = (from + 1 + stream() % 40).min(end);
            let clear_of_ours = from >= at + 8 || to <= at;
            if from < to && clear_of_ours {
                taken.push((0, from..to));
            }
        }
        let wanted = 8 + 1 + stream() % 60;
        match grow_avoiding(&md, "system", wanted * SECTOR_SIZE, &taken) {
            Ok(grown) => {
                let partition = grown.partition("system").expect("the partition it grew");
                assert_eq!(
                    partition.extents[0],
                    Extent::Linear {
                        sectors: 8,
                        device: 0,
                        sector: at
                    },
                    "round {round}: what was there is not moved"
                );
                assert_eq!(partition.sectors(), wanted, "round {round}: the size it was asked for");
                let mine: Vec<(usize, std::ops::Range<u64>)> = sectors(&grown);
                for (index, (device, range)) in mine.iter().enumerate() {
                    assert!(range.start >= start && range.end <= end, "round {round}: {range:?} is inside the device");
                    for (other_device, held) in &taken {
                        assert!(
                            device != other_device || range.end <= held.start || held.end <= range.start,
                            "round {round}: {range:?} is clear of what another slot holds, {held:?}"
                        );
                    }
                    for (other_device, other) in &mine[..index] {
                        assert!(
                            device != other_device || range.end <= other.start || other.end <= range.start,
                            "round {round}: {range:?} does not overlap {other:?}"
                        );
                    }
                }
                // What it planned has to be writable as it stands.
                encode_metadata(&grown).unwrap_or_else(|e| panic!("round {round}: {e:#}"));
            }
            Err(e) => {
                let said = format!("{e:#}");
                assert!(
                    said.contains("not enough") || said.contains("group"),
                    "round {round}: refused for a reason it does not have: {said}"
                );
            }
        }
    }
}

#[test]
fn fragmented_growth_uses_sorted_holes_and_other_devices() {
    let mut md = metadata();
    md.geometry.metadata_max_size = 8192;
    md.block_devices[0].alignment = 0;
    md.block_devices.push(BlockDevice {
        partition_name: "extra".into(),
        first_logical_sector: 0,
        alignment: 0,
        alignment_offset: 0,
        size: 32 * SECTOR_SIZE,
    });
    md.partitions.push(Partition {
        name: "other".into(),
        attributes: 0,
        group: 0,
        extents: vec![
            Extent::Linear {
                sectors: 2,
                device: 0,
                sector: 172,
            },
            Extent::Linear {
                sectors: 2,
                device: 0,
                sector: 190,
            },
            Extent::Linear {
                sectors: 2,
                device: 1,
                sector: 4,
            },
        ],
    });
    md.groups[0].maximum_size = 0;
    let grown = grow(&md, "system", 22 * SECTOR_SIZE).unwrap();
    assert_eq!(
        grown.partition("system").unwrap().extents[1..],
        [
            Extent::Linear {
                sectors: 4,
                device: 0,
                sector: 168
            },
            Extent::Linear {
                sectors: 10,
                device: 0,
                sector: 174
            },
        ]
    );
}

#[test]
fn growth_can_continue_on_another_device() {
    let mut md = metadata();
    md.block_devices[0].alignment = 0;
    md.block_devices[0].first_logical_sector = 160;
    md.block_devices[0].size = 16 * SECTOR_SIZE;
    md.block_devices.push(BlockDevice {
        partition_name: "extra".into(),
        first_logical_sector: 10,
        alignment: 0,
        alignment_offset: 0,
        size: 32 * SECTOR_SIZE,
    });
    md.groups[0].maximum_size = 0;
    let grown = grow(&md, "system", 24 * SECTOR_SIZE).unwrap();
    assert_eq!(
        grown.partition("system").unwrap().extents[1..],
        [
            Extent::Linear {
                sectors: 8,
                device: 0,
                sector: 168
            },
            Extent::Linear {
                sectors: 8,
                device: 1,
                sector: 10
            },
        ]
    );
}

#[test]
fn alignment_skips_unusable_bytes() {
    // Sector 168 is already aligned. An extent ending at 169 forces a gap.
    let mut md = metadata();
    md.partitions.push(Partition {
        name: "guard".into(),
        attributes: 0,
        group: 0,
        extents: vec![Extent::Linear {
            sectors: 1,
            device: 0,
            sector: 169,
        }],
    });
    md.groups[0].maximum_size = 0;
    let grown = grow(&md, "system", 24 * SECTOR_SIZE).unwrap();
    assert_eq!(
        grown.partition("system").unwrap().extents[1..],
        [
            Extent::Linear {
                sectors: 1,
                device: 0,
                sector: 168
            },
            Extent::Linear {
                sectors: 15,
                device: 0,
                sector: 176
            },
        ]
    );
}

#[test]
fn rejects_invalid_growth_requests() {
    let md = metadata();
    assert!(grow(&md, "system", 8 * SECTOR_SIZE)
        .unwrap_err()
        .to_string()
        .contains("not greater"));
    assert!(grow(&md, "system", 8 * SECTOR_SIZE + 1)
        .unwrap_err()
        .to_string()
        .contains("whole number"));
    assert!(grow(&md, "missing", 100 * SECTOR_SIZE).is_err());
    assert!(grow(&md, "system", 1000 * SECTOR_SIZE)
        .unwrap_err()
        .to_string()
        .contains("group"));
}

#[test]
fn rejects_capacity_overflow_before_commit() {
    let md = metadata();
    md.partitions
        .iter()
        .try_for_each(|_| Ok::<(), ()>(()))
        .unwrap();
    let grown = grow(&md, "system", 256 * SECTOR_SIZE).unwrap_err();
    // Group limit fires first; unlimited still exceeds physical capacity.
    let mut md = metadata();
    md.groups[0].maximum_size = 0;
    assert!(grow(&md, "system", 1000 * SECTOR_SIZE).is_err());
    drop(grown);

    let mut huge = metadata();
    huge.partitions = (0..100)
        .map(|i| Partition {
            name: format!("p{i}"),
            attributes: 0,
            group: 0,
            extents: vec![Extent::Zero { sectors: 1 }],
        })
        .collect();
    assert!(encode_metadata(&huge)
        .unwrap_err()
        .to_string()
        .contains("slot holds"));
}

#[test]
fn rejects_overlap_and_bad_alignment() {
    let mut md = metadata();
    md.partitions[0].extents.push(Extent::Linear {
        sectors: 8,
        device: 0,
        sector: 160,
    });
    assert!(encode_metadata(&md)
        .unwrap_err()
        .to_string()
        .contains("overlap"));
    let mut md = metadata();
    md.block_devices[0].alignment = 100;
    assert!(grow(&md, "system", 16 * SECTOR_SIZE)
        .unwrap_err()
        .to_string()
        .contains("alignment"));
}

#[test]
fn commits_primary_and_backup_and_preserves_other_slots() {
    let md = metadata();
    let grown = grow(&md, "system", 24 * SECTOR_SIZE).unwrap();
    let path = scratch("commit");
    let mut disk = vec![0x5a; 256 * 1024];
    let geometry = encode_geometry(&md.geometry).unwrap();
    disk[4096..8192].copy_from_slice(&geometry);
    disk[8192..12288].copy_from_slice(&geometry);
    std::fs::write(&path, &disk).unwrap();
    commit(&path, 1, &grown).unwrap();
    let decoded = read(&path, 1).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    std::fs::remove_file(path).unwrap();
    assert_eq!(decoded, grown);
    let size = md.geometry.metadata_max_size as usize;
    assert_eq!(
        &bytes[12288 + size..12288 + 2 * size],
        &bytes[12288 + 3 * size..12288 + 4 * size]
    );
    assert!(bytes[12288..12288 + size].iter().all(|b| *b == 0x5a));
}
