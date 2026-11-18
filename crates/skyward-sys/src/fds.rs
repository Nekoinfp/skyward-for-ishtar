//! Descriptors and socket addresses, as the kernel takes them.
//!
//! A service the recovery starts may be handed listening sockets in
//! `ANDROID_SOCKET_<name>`, as init hands them: the number in that variable has
//! to be open in the program when it looks it up. The recovery duplicates the
//! descriptor close-on-exec so that nothing else it starts inherits it, and the
//! child clears that for its own exec alone — this module is that clearing,
//! and the address a socket is bound at.

use std::io;
use std::path::Path;

/// Clear close-on-exec on `fds`, for a program that is about to be its
/// `ANDROID_SOCKET_*` variables' own.
pub fn hand_over(fds: &[libc::c_int]) -> io::Result<()> {
    for fd in fds {
        if unsafe { libc::fcntl(*fd, libc::F_SETFD, 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// A Unix-domain socket address and its exact length, without trailing NULs.
pub fn socket_address(path: &Path) -> (libc::sockaddr_un, libc::socklen_t) {
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (slot, byte) in address.sun_path.iter_mut().zip(bytes) {
        *slot = *byte as _;
    }
    let length = (std::mem::size_of::<libc::sa_family_t>() + bytes.len() + 1) as libc::socklen_t;
    (address, length)
}

#[cfg(test)]
mod tests {
    use super::{hand_over, socket_address};
    use std::os::fd::AsRawFd;
    use std::path::Path;
    use std::os::unix::process::CommandExt;

    /// The listener a program is given has to be open in the program: the
    /// recovery duplicates it close-on-exec so that nothing else gets it, and
    /// the child clears that for its own exec alone. Without the clearing the
    /// number the environment names is closed by the time the program looks
    /// for it, which is how a secure-world daemon ends at once and is started
    /// again every few seconds.
    #[test]
    fn the_listener_a_program_is_given_is_open_in_it() {
        let file = std::fs::File::open("/dev/null").expect("open /dev/null");
        let fd = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 50) };
        assert!(fd >= 50, "the copy is at or above 50, where the environment names it");
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg(format!("test -e /proc/self/fd/{fd}"));
        unsafe { cmd.pre_exec(move || hand_over(&[fd])) };
        let status = cmd.status().expect("run sh");
        assert!(status.success(), "the program finds the descriptor it was handed");
    }

    /// Without the clearing the same lookup fails, which is what the test
    /// above is worth only if it can fail.
    #[test]
    fn a_descriptor_left_close_on_exec_is_not_open_in_the_program() {
        let file = std::fs::File::open("/dev/null").expect("open /dev/null");
        let fd = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 50) };
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg(format!("test -e /proc/self/fd/{fd}"));
        let status = cmd.status().expect("run sh");
        assert!(!status.success(), "the descriptor was closed by the exec");
    }

    /// The address a service's socket is bound at: the path, and the exact
    /// length of it, so that bytes after the name are not read as part of it.
    #[test]
    fn the_socket_address_is_the_path_and_its_length() {
        let path = Path::new("/dev/socket/hlos_mink_opener");
        let (address, length) = socket_address(path);
        assert_eq!(address.sun_family, libc::AF_UNIX as libc::sa_family_t);
        let bytes: Vec<u8> = address.sun_path.iter().take(path.as_os_str().len()).map(|c| *c as u8).collect();
        assert_eq!(bytes, path.as_os_str().as_encoded_bytes());
        assert_eq!(length as usize, 2 + path.as_os_str().len() + 1, "a family, the path, and the byte that ends it");
        assert_eq!(address.sun_path[path.as_os_str().len()], 0);
    }
}
