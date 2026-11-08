//! How much memory b33p holds, in the log: a `mem:` line one minute after start, then every 15
//! minutes. Private memory is what b33p alone costs the computer (shared libraries and mapped
//! files left out), and what sigwin measured its renderers by (`docs/decisions.md` there), so
//! the two clients compare on one number.

use std::time::Duration;

/// Starts the thread that logs it; it lives as long as b33p.
pub fn watch() {
    let spawned = std::thread::Builder::new().name("memory".into()).spawn(|| {
        std::thread::sleep(Duration::from_secs(60));
        loop {
            log();
            std::thread::sleep(Duration::from_secs(15 * 60));
        }
    });
    if let Err(error) = spawned {
        tracing::debug!(%error, "no memory log");
    }
}

fn log() {
    match private_bytes() {
        Some(bytes) => tracing::info!("mem: private {:.1} MiB", bytes as f64 / MIB),
        None => tracing::debug!("mem: unknown on this system"),
    }
}

const MIB: f64 = 1024.0 * 1024.0;

/// Memory only this process uses, in bytes.
#[cfg(target_os = "linux")]
fn private_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    anonymous_resident(&status)
}

/// `RssAnon` from `/proc/self/status`: resident memory not backed by a file, which is what
/// the process itself allocated.
#[cfg(any(target_os = "linux", test))]
fn anonymous_resident(status: &str) -> Option<u64> {
    let line = status.lines().find(|line| line.starts_with("RssAnon:"))?;
    let kib: u64 = line
        .trim_start_matches("RssAnon:")
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .ok()?;
    Some(kib * 1024)
}

/// The commit charge of the process (`PrivateUsage`), as Task Manager's "Memory" column shows.
#[cfg(windows)]
fn private_bytes() -> Option<u64> {
    use windows::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows::Win32::System::Threading::GetCurrentProcess;

    let mut counters = PROCESS_MEMORY_COUNTERS_EX {
        cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    // SAFETY: the counters are the size `cb` says, and live through the call; the process
    // handle is the pseudo handle, which needs no closing.
    unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&raw mut counters).cast::<PROCESS_MEMORY_COUNTERS>(),
            counters.cb,
        )
    }
    .ok()?;
    Some(counters.PrivateUsage as u64)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn private_bytes() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_anonymous_resident_memory() {
        let status =
            "Name:\tb33p\nVmRSS:\t  120000 kB\nRssAnon:\t   43008 kB\nRssFile:\t 70000 kB\n";
        assert_eq!(anonymous_resident(status), Some(43008 * 1024));
        assert_eq!(anonymous_resident("Name:\tb33p\n"), None);
    }
}
