//! The phone's own snapshot daemon, which is what actually serves the blocks
//! of a compressed snapshot.
//!
//! The kernel's `user` target sends each request to `/dev/dm-user/<name>`, and
//! the program that reads that device is `snapuserd`, out of the installed
//! system. It is started here, asked whether it is serving, and stopped again:
//! nothing is mounted and nothing is written.
//!
//! The messages are the ones in [`skyward_vab::dmuser::Request`], which were
//! read from AOSP and held against this phone's own daemon: of the thirteen it
//! knows, twelve appear in that binary verbatim.

use anyhow::{bail, ensure, Context, Result};
use skyward_vab::dmuser::{Reply, Request};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Where the installed system keeps the daemon. It is statically linked, so it
/// needs nothing of that system's linker to run.
const DAEMON: &str = "/mnt/system/system/bin/snapuserd";

/// Where the daemon listens. Android's own client asks for a socket "in the
/// reserved namespace", which on this phone is a file under `/dev/socket`; the
/// daemon's own descriptors name it, and this is that name.
const SOCKET: &str = "/dev/socket/snapuserd";

/// How long the daemon is given to start and answer.
const PATIENCE: Duration = Duration::from_secs(10);

/// Ask the phone's snapshot daemon whether it is serving, starting it if it is
/// not, and leave it stopped.
pub fn probe() -> Result<()> {
    let was_running = connect().is_ok();
    println!("the daemon at {DAEMON}: {}", if was_running { "already serving" } else { "not running" });
    let mut started: Option<Child> = None;
    if !was_running {
        ensure!(Path::new(DAEMON).is_file(), "{DAEMON} is not there: is the installed system mounted at /mnt/system?");
        // `-user_snapshot` is what makes it serve compressed snapshots rather
        // than the older in-kernel ones; the rest are the daemon's own.
        let child = Command::new(DAEMON)
            .args(["-user_snapshot", "-o_direct", "-worker_count=2"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("start {DAEMON}"))?;
        started = Some(child);
    }
    let result = ask();
    if let Some(mut child) = started {
        // It is asked politely first, and taken away if it does not go.
        if let Ok(mut stream) = connect() {
            let _ = stream.write_all(Request::Stop.text().as_bytes());
        }
        std::thread::sleep(Duration::from_millis(300));
        let _ = child.kill();
        let said = child.wait_with_output().map(|out| String::from_utf8_lossy(&out.stderr).into_owned()).unwrap_or_default();
        for line in said.lines().filter(|line| !line.trim().is_empty()).take(8) {
            println!("  it said: {}", line.trim());
        }
    }
    result
}

/// What the daemon answers to `query`, and to the messages that say what it
/// would be asked to serve.
fn ask() -> Result<()> {
    let mut stream = wait_for_daemon()?;
    say(&mut stream, &Request::Query)?;
    let reply = hear(&mut stream)?;
    println!("  query: {reply:?}");
    match reply {
        Reply::Active => println!("the daemon is active and would serve blocks"),
        Reply::Passive => println!("the daemon is passive: it is on its way out"),
        other => bail!("the daemon answered {other:?} rather than being active"),
    }
    // Whether it knows the message that matters for a compressed snapshot:
    // telling it which copy-on-write device to serve. The answer's shape is
    // what is being read here, not the device named in it.
    say(&mut stream, &Request::Init { misc: "skyward-probe".into(), cow: "/dev/null".into(), backing: "/dev/null".into(), merge_target: None })?;
    match hear(&mut stream)? {
        Reply::Sectors(sectors) => println!("  it answered how many sectors a copy-on-write device has: {sectors}"),
        Reply::Other(text) => println!("  it answered {text:?}, which is its own word for refusing a device that is not a snapshot"),
        other => println!("  it answered {other:?}"),
    }
    Ok(())
}

/// A connection to the daemon, waited for while it comes up.
fn wait_for_daemon() -> Result<UnixStream> {
    let until = Instant::now() + PATIENCE;
    loop {
        match connect() {
            Ok(stream) => return Ok(stream),
            Err(e) if Instant::now() < until => {
                let _ = e;
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e).context("connect to the snapshot daemon"),
        }
    }
}

/// A connection to the daemon at the socket it listens on.
fn connect() -> Result<UnixStream> {
    let stream = UnixStream::connect(SOCKET).with_context(|| format!("connect to {SOCKET}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    Ok(stream)
}

fn say(stream: &mut UnixStream, request: &Request) -> Result<()> {
    let text = request.text();
    stream.write_all(text.as_bytes()).with_context(|| format!("send {text:?}"))?;
    // The daemon reads a message, not a line: no ending is sent.
    Ok(())
}

fn hear(stream: &mut UnixStream) -> Result<Reply> {
    let mut held = [0u8; 256];
    let read = stream.read(&mut held).context("read the daemon's answer")?;
    ensure!(read > 0, "the daemon closed the connection rather than answering");
    Ok(Reply::parse(&String::from_utf8_lossy(&held[..read])))
}

/// What the daemon says about each snapshot it serves, and — when asked — what
/// it says while merging them.
///
/// The global `merge_percent` only says how far the lot has come; the per
/// partition `getstatus` is what names a partition that has stopped.
pub fn diagnose(merge: bool) -> Result<()> {
    ensure!(skyward_vab::snapuserd_client::reachable(), "the snapshot daemon is not serving: is anything mapped?");
    let names = std::fs::read_dir("/metadata/ota/snapshots")
        .context("read the snapshots the update left")?
        .filter_map(|entry| entry.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect::<Vec<_>>();
    for name in &names {
        let said = skyward_vab::snapuserd_client::ask(&format!("getstatus,user_snapshot-{name}")).unwrap_or_else(|e| format!("<{e}>"));
        println!("  {name:24} {said}");
    }
    println!("  overall merge: {}", skyward_vab::snapuserd_client::ask("merge_percent").unwrap_or_else(|e| e.to_string()));
    if !merge {
        return Ok(());
    }
    for name in &names {
        let misc = format!("user_snapshot-{name}");
        let said = skyward_vab::snapuserd_client::ask(&format!("initiate_merge,{misc}")).unwrap_or_else(|e| format!("<{e}>"));
        println!("initiate {misc}: {said}");
    }
    for round in 0..24 {
        std::thread::sleep(Duration::from_secs(5));
        let percent = skyward_vab::snapuserd_client::ask("merge_percent").unwrap_or_else(|e| e.to_string());
        let mut done = 0;
        let mut failed = Vec::new();
        for name in &names {
            match skyward_vab::snapuserd_client::ask(&format!("getstatus,user_snapshot-{name}")) {
                Ok(said) if said.contains("complete") => done += 1,
                Ok(said) => failed.push(format!("{name}={said}")),
                Err(e) => failed.push(format!("{name}=<{e}>")),
            }
        }
        println!("  [{}] merge {percent}, complete {done}/{}, {}", round * 5 + 5, names.len(), failed.join(" "));
        if done == names.len() {
            break;
        }
    }
    Ok(())
}