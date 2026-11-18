//! What the report `skyward check` prints says, tried without a phone.

use super::*;

fn entry(line: &str) -> skyward_block::fstab::Entry {
    skyward_block::fstab::entries(line).into_iter().next().expect("one entry")
}

/// A report of a phone whose table says what OS4's does.
fn gathered(line: &str) -> Gathered {
    Gathered {
        codename: "xuanyuan".into(),
        packed_on: Some(("OS4.0.4.0.XOACNXM".into(), "55f491052a360bc1".into())),
        slot: Some('B'),
        installed: Some(("OS4.0.4.0.XOACNXM".into(), "15".into(), "2026-10-01".into())),
        tables: vec![("/vendor/etc/fstab.qcom", true), ("/odm/etc/fstab.qcom", false)],
        data: Some(entry(line)),
        key_dir: Some("/metadata/vold/metadata_encryption/key".into()),
        key_files: Some(vec![("key".into(), 64)]),
        read_through: Some("/dev/block/by-name/userdata".into()),
        services: vec![
            ("android.hardware.security.keymint.IKeyMintDevice/default".into(), true),
            ("android.hardware.weaver.IWeaver/default".into(), false),
        ],
        update: Some("None".into()),
        logs: vec![(
            "/skyward/run/services/android.hardware.security.keymint-service-qti.log".into(),
            vec!["CANNOT LINK EXECUTABLE: library \"libvndksupport.so\" not found".into()],
        )],
        ..Gathered::default()
    }
}

const ENCRYPTED: &str = "/dev/block/bootdevice/by-name/userdata /data f2fs noatime,nosuid fileencryption=ice,wrappedkey,keydirectory=/metadata/vold/metadata_encryption,metadata_encryption=aes-256-xts:wrappedkey_v0";

#[test]
fn the_report_names_the_parts_the_data_partition_needs() {
    let said = report(&gathered(ENCRYPTED));
    for wanted in [
        "for xuanyuan",
        "packed on OS4.0.4.0.XOACNXM, from the recovery partition 55f491052a360bc1…",
        "running from slot B",
        "installed system OS4.0.4.0.XOACNXM (Android 15, patch 2026-10-01)",
        "/vendor/etc/fstab.qcom: there",
        "/odm/etc/fstab.qcom: not there",
        "device: /dev/block/bootdevice/by-name/userdata",
        "filesystem: f2fs",
        "metadata_encryption: aes-256-xts:wrappedkey_v0",
        "keydirectory: /metadata/vold/metadata_encryption",
        "fileencryption: ice",
        "the key it keeps: /metadata/vold/metadata_encryption/key: key (64 bytes)",
        "/data is read through: /dev/block/by-name/userdata",
        "registered: android.hardware.security.keymint.IKeyMintDevice/default",
        "NOT registered: android.hardware.weaver.IWeaver/default",
        "an update in flight: None",
        "| CANNOT LINK EXECUTABLE",
    ] {
        assert!(said.contains(wanted), "the report does not say {wanted:?}:\n{said}");
    }
    assert!(!said.contains("no metadata_encryption"), "an encrypted partition is not noted as plain");
}

#[test]
fn a_table_that_says_the_partition_is_not_encrypted_whole_says_so() {
    let plain = "/dev/block/bootdevice/by-name/userdata /data f2fs noatime fileencryption=ice,wrappedkey,keydirectory=/metadata/vold/metadata_encryption";
    let said = report(&gathered(plain));
    assert!(said.contains("note: no metadata_encryption"), "{said}");
    assert!(said.contains("metadata_encryption: (none)"), "what it does not say is shown as nothing:\n{said}");
}

#[test]
fn a_look_that_cannot_be_made_says_which_one_and_the_rest_still_print() {
    let mut g = gathered(ENCRYPTED);
    g.installed = None;
    g.data = None;
    g.data_error = Some("read /vendor/etc/fstab.qcom: No such file or directory".into());
    g.key_dir = None;
    g.key_files = None;
    g.key_error = Some("the phone says where the key for its data partition is not".into());
    g.services.clear();
    g.update = None;
    g.update_error = Some("misc: no such file".into());
    g.logs.clear();
    let said = report(&g);
    for wanted in [
        "installed system: its build properties could not be read",
        "it could not be read: read /vendor/etc/fstab.qcom",
        "the key it keeps: (the phone says where the key for its data partition is not)",
        "(the configuration names none)",
        "an update in flight: (misc: no such file)",
        "running from slot B",
        "packed on OS4.0.4.0.XOACNXM",
    ] {
        assert!(said.contains(wanted), "the report does not say {wanted:?}:\n{said}");
    }
    assert!(!said.contains("what the services said"), "no log is invented when none was kept");
}

#[test]
fn an_empty_key_directory_is_told_apart_from_a_full_one() {
    let mut g = gathered(ENCRYPTED);
    g.key_files = Some(Vec::new());
    assert!(report(&g).contains("metadata_encryption/key: empty"), "{}", report(&g));
}

#[test]
fn a_kept_log_gives_its_last_lines_and_leaves_the_blank_ones_out() {
    let mut text = String::new();
    for i in 0..SAID_LINES + 5 {
        text.push_str(&format!("line {i}\n"));
        text.push('\n');
    }
    let kept = kept_lines(&text);
    assert_eq!(kept.len(), SAID_LINES, "only the last lines are kept");
    assert_eq!(kept.first().map(String::as_str), Some("line 5"), "and they are the last ones");
    assert!(kept.iter().all(|line| !line.is_empty()), "blank lines are left out");
    assert!(kept_lines("short\n").len() == 1, "a short log is kept whole");
}
