//! Make the secure world see the version of the system that made the keys.
//!
//! A key made under one build is refused by a key service that started while
//! a recovery's own, older build's version properties were set. Starting the
//! key service again, with those four values put back to the installed
//! system's own, is what Android's recovery does before it opens the data
//! partition. No key is renewed and nothing is written back: a key that is
//! still refused is left exactly as it was.

use anyhow::{ensure, Context, Result};
use std::time::{Duration, Instant};

const KEYMINT: &str = "vendor.keymint-qti";
const KEYSTORE: &str = "keystore2";

const BUILD_VERSION: [&str; 4] = [
    "ro.build.version.release",
    "ro.build.version.release_or_codename",
    "ro.build.version.security_patch",
    "ro.vendor.build.security_patch",
];

/// What the key service is being told, for a message that names the values
/// alongside the HAL's own answer.
pub fn said() -> String {
    BUILD_VERSION
        .iter()
        .map(|name| format!("{name}={}", skyward_sys::prop(name).unwrap_or_else(|| "(unset)".into())))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Which key service is running, as the hardware service names it.
pub fn hardware() -> String {
    let binder = match skyward_binder::driver::Binder::open() {
        Ok(binder) => binder,
        Err(e) => return format!("the key driver cannot be opened: {e:#}"),
    };
    match super::keymint::KeyMint::open(&binder).and_then(|service| service.hardware()) {
        Ok(h) => format!("KeyMint {} at {:?}, {}", h.version, h.security_level, h.name),
        Err(e) => format!("the key service does not answer about itself: {e:#}"),
    }
}

/// Stop the key store and the key service, then start them in that order and
/// wait for the key service to register. The values are read from the
/// properties, which the recovery set from the installed system's build.prop
/// files before the services first started. An empty value means the version
/// was never adopted; saying so beats letting the secure world answer a
/// refusal no one can read.
pub fn pin() -> Result<()> {
    for name in BUILD_VERSION {
        let value = skyward_sys::prop(name);
        ensure!(value.is_some_and(|v| !v.is_empty()), "no value for {name} to give the key service: {}", said());
        skyward_sys::kmsg(&format!("key service environment: {name}={}", value.unwrap_or_default()))?;
    }
    for service in [KEYSTORE, KEYMINT] {
        let _ = skyward_sys::set_prop("ctl.stop", service);
    }
    let _ = skyward_sys::wait_prop(&format!("init.svc.{KEYMINT}"), "stopped", Duration::from_secs(5));
    skyward_sys::set_prop("ctl.start", KEYMINT).context("start the key service again")?;
    let binder = skyward_binder::driver::Binder::open().context("open the key driver after the restart")?;
    let until = Instant::now() + Duration::from_secs(10);
    while skyward_binder::service::find(&binder, super::keymint::SERVICE)?.is_none() {
        ensure!(Instant::now() < until, "the key service did not register after it was started again");
        std::thread::sleep(Duration::from_millis(50));
    }
    skyward_sys::kmsg("the key service registered again with the installed system's version")?;
    skyward_sys::set_prop("ctl.start", KEYSTORE).context("start the key store again")?;
    Ok(())
}
