//! The phone's own snapshot daemon, which serves the blocks of a compressed
//! snapshot.
//!
//! The kernel's `user` target sends each request to `/dev/dm-user/<name>`, and
//! the program reading that device is `snapuserd` out of the installed system.
//! It is statically linked, so it needs nothing of that system's linker to run
//! inside a recovery.
//!
//! The messages are the ones in [`crate::dmuser::Request`], read from AOSP and
//! held against this phone's own binary: of the thirteen it knows, twelve
//! appear in it verbatim. Where it listens is not the abstract namespace the
//! client asks for by name but a file, `/dev/socket/snapuserd`, as that
//! binary's own descriptors show.

use crate::dmuser::{Reply, Request};
use anyhow::{bail, ensure, Context, Result};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Where the daemon may be, the recovery's own first: it has to run before
/// any partition is mounted, and a compressed snapshot is the system partition
/// itself. The installed system's copy is the fallback, for a recovery
/// carrying none of its own.
pub const CANDIDATES: [&str; 2] = ["/system/bin/snapuserd", "/mnt/system/system/bin/snapuserd"];

/// Where it listens.
pub const SOCKET: &str = "/dev/socket/snapuserd";

/// How long the daemon is given to come up.
const PATIENCE: Duration = Duration::from_secs(10);

/// Start the daemon unless it is already serving, and wait until it answers.
pub fn ensure_running() -> Result<()> {
    if connect().is_ok() {
        return Ok(());
    }
    let daemon = CANDIDATES.iter().find(|path| Path::new(path).is_file()).with_context(|| {
        format!("no snapshot daemon: none of {CANDIDATES:?} is there")
    })?;
    // `-user_snapshot` is what makes it serve compressed snapshots rather than
    // the older in-kernel ones. It is left running: the device it serves needs
    // it for as long as the device is mapped.
    Command::new(daemon)
        .args(["-user_snapshot", "-o_direct", "-worker_count=2"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("start {daemon}"))?;
    let until = Instant::now() + PATIENCE;
    loop {
        if connect().is_ok() {
            return Ok(());
        }
        ensure!(Instant::now() < until, "{daemon} did not start listening within {} seconds", PATIENCE.as_secs());
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Whether the daemon is there and serving.
pub fn reachable() -> bool {
    connect().is_ok()
}

/// Tell the daemon which copy-on-write device serves `misc`, and what it reads
/// through, and give how many sectors the device it made has.
///
/// The daemon is started first if it is not serving: a recovery has no init
/// script to start it, so the first device that needs it brings it up.
pub fn init_cow(misc: &str, cow: &Path, backing: &Path, merge_target: Option<&Path>) -> Result<u64> {
    ensure_running()?;
    let request = Request::Init {
        misc: misc.to_string(),
        cow: cow.display().to_string(),
        backing: backing.display().to_string(),
        merge_target: merge_target.map(|p| p.display().to_string()),
    };
    match exchange(&request)? {
        Reply::Sectors(sectors) => Ok(sectors),
        other => bail!("the daemon answered {other:?} rather than the size of {misc}"),
    }
}

/// Tell the daemon to begin serving `misc`.
pub fn start(misc: &str) -> Result<()> {
    match exchange(&Request::Start { misc: misc.to_string() })? {
        Reply::Done => Ok(()),
        other => bail!("the daemon answered {other:?} rather than starting {misc}"),
    }
}

/// Tell the daemon to begin merging `misc` into the partition it stands for.
///
/// Merging a compressed snapshot is the daemon's own work: the kernel's
/// `snapshot-merge` target never sees such a device, and the blocks have to be
/// decompressed on the way into the partition.
pub fn initiate_merge(misc: &str) -> Result<()> {
    match exchange(&Request::InitiateMerge { misc: misc.to_string() })? {
        Reply::Done => Ok(()),
        other => bail!("the daemon answered {other:?} rather than beginning to merge {misc}"),
    }
}

/// How far the merge has come, as a percentage of the whole.
pub fn merge_percent() -> Result<f64> {
    match exchange(&Request::MergePercent)? {
        Reply::Percent(percent) => Ok(percent),
        other => bail!("the daemon answered {other:?} rather than how far the merge has come"),
    }
}

/// Ask the daemon whether it is serving.
pub fn query() -> Result<Reply> {
    exchange(&Request::Query)
}

/// Send `text` exactly as given and give what comes back, for the messages
/// that have no shape of their own here: `getstatus`, `pause_merge` and
/// whatever else the daemon grows.
pub fn ask(text: &str) -> Result<String> {
    let mut stream = connect().context("reach the snapshot daemon")?;
    stream.write_all(text.as_bytes()).with_context(|| format!("send {text:?}"))?;
    let mut held = [0u8; 256];
    let read = stream.read(&mut held).with_context(|| format!("read the answer to {text:?}"))?;
    ensure!(read > 0, "the daemon closed the connection rather than answering {text:?}");
    Ok(String::from_utf8_lossy(&held[..read]).into_owned())
}

fn exchange(request: &Request) -> Result<Reply> {
    let mut stream = connect().context("reach the snapshot daemon")?;
    let text = request.text();
    stream.write_all(text.as_bytes()).with_context(|| format!("send {text:?}"))?;
    // The daemon reads a message, not a line: nothing is sent after it.
    let mut held = [0u8; 256];
    let read = stream.read(&mut held).with_context(|| format!("read the answer to {text:?}"))?;
    ensure!(read > 0, "the daemon closed the connection rather than answering {text:?}");
    Ok(Reply::parse(&String::from_utf8_lossy(&held[..read])))
}

fn connect() -> Result<UnixStream> {
    let stream = UnixStream::connect(SOCKET).with_context(|| format!("connect to {SOCKET}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    Ok(stream)
}
