// Ask the phone maker's update server about this phone, the way the recovery
// does, and print what it says. Run from the workspace root:
//
//     cargo run -p skyward-net --example ask -- ishtar OS4.0.29.0.XEOCNXM 17
//
// The question is sealed with the same key the stock updater seals it with,
// so this asks exactly what the phone would ask.
use skyward_net::release::{self, Installed};

fn main() {
    let mut args = std::env::args().skip(1);
    let device = args.next().unwrap_or_else(|| "ishtar".to_string());
    let version = args.next().unwrap_or_else(|| "OS4.0.29.0.XEOCNXM".to_string());
    let android = args.next().unwrap_or_else(|| "17".to_string());
    let installed = Installed { device: device.clone(), version: version.clone(), android: android.clone(), unlocked: true };
    println!("问: device={device} version={version} android={android}");
    // ureq's own agent, so this runs on a computer with the system's resolver
    // rather than the recovery's, which asks the network it joined.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(20)))
        .timeout_per_read(Some(std::time::Duration::from_secs(40)))
        .http_status_as_error(false)
        .build()
        .into();
    match release::ask(&agent, "https://update.miui.com/updates/miotaV3.php", &installed) {
        Ok(Some(release)) => {
            println!("答: 有更新");
            println!("  版本   {}", release.version);
            println!("  名称   {}", release.name);
            println!("  大小   {}", release.size);
            for url in &release.urls {
                println!("  下载   {url}");
            }
        }
        Ok(None) => println!("答: 没有这台手机的更新"),
        Err(e) => println!("失败: {e:#}"),
    }
}
