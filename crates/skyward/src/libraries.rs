//! Copying the phone's own libraries into the folder the recovery's programs
//! load from.
//!
//! This is what lets the phone's hardware services start inside a recovery at
//! all, and it is the one step of that which is plain file work: it is kept
//! out of the phone-only module so that it can be tried with the layout the
//! phone really has — two libraries that live in two different folders.

use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

/// Copy `names` into `into`, each from the first of `sources` that holds it,
/// skipping the ones already there, and give how many were copied.
///
/// A name no source holds is passed over: a recovery that cannot find one of
/// them is worse off, but not for the others' sake.
///
/// Each is copied beside where it goes and then renamed into place, so a
/// program starting at the same moment never reads half a library.
pub fn copy(names: &[&str], sources: &[&Path], into: &Path) -> Result<usize> {
    let mut copied = 0;
    for name in names {
        let to = into.join(name);
        if to.exists() {
            // A library an earlier run of the recovery put there is as good
            // as the one on the system, which does not change under it.
            continue;
        }
        let Some(from) = sources.iter().map(|dir| dir.join(name)).find(|path| path.is_file()) else { continue };
        let mut part = to.clone().into_os_string();
        part.push(".part");
        fs::copy(&from, &part).with_context(|| format!("copy {} to {}", from.display(), Path::new(&part).display()))?;
        fs::rename(&part, &to).with_context(|| format!("put {} in place", to.display()))?;
        copied += 1;
    }
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::copy;
    use std::path::Path;

    /// A folder of libraries, with `names` in it.
    fn dir(root: &Path, name: &str, names: &[&str]) -> std::path::PathBuf {
        let at = root.join(name);
        std::fs::create_dir_all(&at).expect("make a folder");
        for lib in names {
            std::fs::write(at.join(lib), lib.as_bytes()).expect("write a library");
        }
        at
    }

    #[test]
    fn the_two_libraries_come_from_the_two_folders_the_phone_keeps_them_in() {
        // Measured off the OS4.0.4.0 system partition: libvndksupport.so is in
        // system/lib64 and libdl_android.so only in system/lib64/bootstrap.
        let root = tempfile::tempdir().expect("a folder");
        let lib64 = dir(root.path(), "lib64", &["libvndksupport.so"]);
        let bootstrap = dir(root.path(), "lib64/bootstrap", &["libdl_android.so"]);
        let into = dir(root.path(), "recovery-lib64", &[]);
        let copied = copy(&["libvndksupport.so", "libdl_android.so"], &[&lib64, &bootstrap], &into).expect("copy them");
        assert_eq!(copied, 2, "both came, each from the folder that holds it");
        for lib in ["libvndksupport.so", "libdl_android.so"] {
            assert_eq!(std::fs::read(into.join(lib)).expect("read it back"), lib.as_bytes(), "{lib}");
        }
        assert!(!into.join("libdl_android.so.part").exists(), "nothing is left half copied");
    }

    #[test]
    fn a_library_already_in_place_is_left_alone() {
        let root = tempfile::tempdir().expect("a folder");
        let lib64 = dir(root.path(), "lib64", &["libvndksupport.so"]);
        let into = dir(root.path(), "recovery-lib64", &["libvndksupport.so"]);
        std::fs::write(into.join("libvndksupport.so"), b"an earlier run's copy").expect("write it");
        assert_eq!(copy(&["libvndksupport.so"], &[&lib64], &into).expect("copy it"), 0, "nothing was copied");
        assert_eq!(std::fs::read(into.join("libvndksupport.so")).expect("read it"), b"an earlier run's copy");
    }

    #[test]
    fn a_library_no_folder_holds_is_passed_over_and_the_rest_still_come() {
        let root = tempfile::tempdir().expect("a folder");
        let lib64 = dir(root.path(), "lib64", &["libvndksupport.so"]);
        let into = dir(root.path(), "recovery-lib64", &[]);
        let copied = copy(&["libnothere.so", "libvndksupport.so"], &[&lib64], &into).expect("copy what is there");
        assert_eq!(copied, 1);
        assert!(into.join("libvndksupport.so").exists());
        assert!(!into.join("libnothere.so").exists(), "nothing is made up for a library that is not there");
    }
}
