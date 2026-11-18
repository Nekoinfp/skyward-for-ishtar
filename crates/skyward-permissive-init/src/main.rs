//! The first program of an init_boot ramdisk Skyward made permissive.
//!
//! A user build's init loads the SELinux policy and then turns enforcing
//! on, whatever the kernel's command line or the bootconfig ask. This
//! program, started by the kernel in init's place, forks a watcher and
//! then starts the ramdisk's own first program in its own place, as pid 1
//! with the arguments and environment it was given.
//!
//! The watcher mounts selinuxfs while no policy is loaded yet, opens its
//! `enforce` file, and takes the mount away again, so init's first stage
//! never sees it; the open file outlives the mount. A process the kernel
//! started before any policy is loaded runs in the kernel's own domain,
//! which the policy lets turn enforcing off. So once init loads the policy
//! and turns enforcing on, the watcher writes 0 to the file it still holds.
//! It keeps watching for a short while, for an init that turns it on again,
//! then ends.

#![cfg_attr(target_os = "android", no_main)]

#[cfg(target_os = "android")]
const OWN_INIT: &[u8] = b"/init.skyward\0";
#[cfg(target_os = "android")]
const MOUNT: &[u8] = b"/.skyward_selinux\0";
#[cfg(target_os = "android")]
const ENFORCE: &[u8] = b"/.skyward_selinux/enforce\0";
/// A poll every tenth of a second.
#[cfg(target_os = "android")]
const POLL_US: libc::c_uint = 100_000;
/// How long the watcher keeps polling after it first turns enforcing off,
/// and how long in all, in polls.
#[cfg(target_os = "android")]
const AFTER: u32 = 300;
#[cfg(target_os = "android")]
const LONGEST: u32 = 3000;

#[cfg(target_os = "android")]
#[no_mangle]
unsafe extern "C" fn main(_argc: libc::c_int, argv: *const *const libc::c_char, envp: *const *const libc::c_char) -> libc::c_int {
    unsafe {
        let fd = open_enforce();
        if fd >= 0 {
            match libc::fork() {
                0 => {
                    watch(fd);
                    libc::_exit(0);
                }
                -1 => {
                    libc::close(fd);
                }
                _ => {
                    libc::close(fd);
                }
            }
        }
        libc::execve(OWN_INIT.as_ptr().cast(), argv, envp);
        // A pid 1 that returns stops the kernel, as a missing init does.
        1
    }
}

#[cfg(target_os = "android")]
unsafe fn open_enforce() -> libc::c_int {
    unsafe {
        libc::mkdir(MOUNT.as_ptr().cast(), 0o700);
        if libc::mount(c"selinuxfs".as_ptr().cast(), MOUNT.as_ptr().cast(), c"selinuxfs".as_ptr().cast(), 0, std::ptr::null()) != 0 {
            libc::rmdir(MOUNT.as_ptr().cast());
            return -1;
        }
        let fd = libc::open(ENFORCE.as_ptr().cast(), libc::O_RDWR | libc::O_CLOEXEC);
        libc::umount2(MOUNT.as_ptr().cast(), libc::MNT_DETACH);
        libc::rmdir(MOUNT.as_ptr().cast());
        fd
    }
}

#[cfg(target_os = "android")]
unsafe fn watch(fd: libc::c_int) {
    let mut turned_off_at: Option<u32> = None;
    for tick in 0..LONGEST {
        if let Some(at) = turned_off_at {
            if tick - at >= AFTER {
                break;
            }
        }
        let mut b = [0u8; 1];
        let n = unsafe { libc::pread(fd, b.as_mut_ptr().cast(), 1, 0) };
        if n == 1 && b[0] == b'1'
            && unsafe { libc::pwrite(fd, b"0".as_ptr().cast(), 1, 0) } == 1
            && turned_off_at.is_none()
        {
            turned_off_at = Some(tick);
        }
        unsafe { libc::usleep(POLL_US) };
    }
}

#[cfg(not(target_os = "android"))]
fn main() {
    eprintln!("permissive-init runs only on the phone, as an init_boot ramdisk's first program");
    std::process::exit(1);
}
