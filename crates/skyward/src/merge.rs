//! Merging an update into the slot the phone runs from, as libsnapshot
//! follows a merge: started where it has not begun, then looked at every
//! two seconds until it ends, the bootloader told where it stands through
//! the phone's own boot control. On a thread of its own for the screen.

use anyhow::{Context, Result};
use skyward_binder::driver::Binder;
use skyward_vab::boot::BootControl;
use skyward_vab::{Merge, Slot, Standing};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// How often a merge under way is looked at, as libsnapshot looks.
const LOOK_EVERY: Duration = Duration::from_secs(2);

/// Merge the update into `slot`, starting the merge first where `start`,
/// and follow it to its end, `each` told how every look at it went.
/// /metadata is writable while it runs, and read-only again after.
pub fn run(slot: Slot, start: bool, each: &mut dyn FnMut(&Merge)) -> Result<Merge> {
    let (binder, hardware) = (Binder::open()?, Binder::hardware()?);
    let control = BootControl::open(&binder, &hardware)?;
    let tell = |status| control.set_merge_status(status);
    skyward_vab::metadata_writable(true).context("make /metadata writable to merge the update")?;
    let merged = (|| -> Result<Merge> {
        if start {
            skyward_vab::start_merge(slot, &tell)?;
        }
        loop {
            let merge = skyward_vab::check_merge(slot, &tell)?;
            each(&merge);
            if !matches!(merge, Merge::Going(_)) {
                return Ok(merge);
            }
            thread::sleep(LOOK_EVERY);
        }
    })();
    let back = skyward_vab::metadata_writable(false).context("make /metadata read-only again");
    let merge = merged?;
    back?;
    Ok(merge)
}

/// Merge an update whose snapshots are compressed, through the daemon that
/// serves them. `/metadata` is writable while it runs, and read-only again
/// after; `each` is told how every look at it went, and `watch` what the
/// daemon says of each snapshot by name.
pub fn run_served(slot: Slot, each: &mut dyn FnMut(&Merge), watch: &mut dyn FnMut(&str, &str, f64)) -> Result<Merge> {
    let (binder, hardware) = (Binder::open()?, Binder::hardware()?);
    let control = BootControl::open(&binder, &hardware)?;
    let tell = |status| control.set_merge_status(status);
    skyward_vab::metadata_writable(true).context("make /metadata writable to merge the update")?;
    let merged = (|| -> Result<Merge> {
        let snaps = skyward_vab::snapshots::Snapshots::lock(true)?.context("no update was ever applied")?;
        skyward_vab::snapshots::merge_served(&snaps, slot, &tell, watch)
    })();
    let back = skyward_vab::metadata_writable(false).context("make /metadata read-only again");
    let merge = merged?;
    each(&merge);
    back?;
    Ok(merge)
}
pub enum Event {
    /// How a look at the merge went.
    Looked(Merge),
    /// The merge could not be started or followed, and why.
    Failed(String),
}

/// A merge followed on a thread of its own.
pub struct Merger {
    events: Receiver<Event>,
    /// Readable while events wait.
    wake: OwnedFd,
    thread: Option<JoinHandle<()>>,
}

impl Merger {
    /// Follow the merge into `slot` as [`run`] does, starting it first where
    /// `start`. A compressed update is followed the way only it can be: by
    /// the daemon that serves it.
    pub fn start(slot: Slot, start: bool) -> Result<Merger> {
        let mut fds = [0; 2];
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
            return Err(std::io::Error::last_os_error()).context("make the merger's wake pipe");
        }
        let (wake, signal) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        let (tx, events) = mpsc::channel();
        // Which way this merge runs is decided once, before the thread begins,
        // so that the thread never has to ask anything of the screen. Only a
        // merge being started here can be of the compressed kind: one the
        // recovery only follows was begun by the system, which drives it
        // itself.
        let served = start
            && matches!(
                (skyward_vab::standing(slot), skyward_vab::snapshots_mapped(slot)),
                (Ok(Standing::Compressed), Ok(true))
            );
        let thread = thread::Builder::new()
            .name("merge".into())
            .spawn(move || {
                let tell = |event: Event| {
                    // The screen only stops listening once it has let the
                    // merger go.
                    let _ = tx.send(event);
                    unsafe { libc::write(signal.as_raw_fd(), [1u8].as_ptr().cast(), 1) };
                };
                let result = if served {
                    run_served(slot, &mut |merge| tell(Event::Looked(*merge)), &mut |_, _, _| {})
                } else {
                    run(slot, start, &mut |merge| tell(Event::Looked(*merge)))
                };
                if let Err(e) = result {
                    tell(Event::Failed(format!("{e:#}")));
                }
            })
            .context("start the merger")?;
        Ok(Merger { events, wake, thread: Some(thread) })
    }

    pub fn fd(&self) -> RawFd {
        self.wake.as_raw_fd()
    }

    /// The events since the last call.
    pub fn events(&self) -> Vec<Event> {
        let mut drain = [0u8; 64];
        while unsafe { libc::read(self.wake.as_raw_fd(), drain.as_mut_ptr().cast(), drain.len()) } > 0 {}
        self.events.try_iter().collect()
    }

    /// Whether the merge is no longer followed: it ended, or failed.
    pub fn ended(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }
}

impl Drop for Merger {
    /// The screen lets the merger go once it has ended.
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            thread.join().expect("the merger does not panic");
        }
    }
}
