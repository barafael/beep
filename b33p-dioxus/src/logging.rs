//! Logging to the system log, with user data scrubbed out of every line.
//!
//! Stricter than the desktop's (`b33p-platform`'s `logging`): `info` and above only, and no
//! `RUST_LOG` to lift that, because a log line is already formatted by the time a filter could
//! redact it, and the libraries below `info` do log message contents. `b33p_core` itself may
//! say more in a debug build. Android sends what a process writes to stderr to logcat.

use std::io::Write;

use tracing_subscriber::fmt::MakeWriter;

pub fn init() {
    let filter = if cfg!(debug_assertions) {
        "info,b33p_core=debug"
    } else {
        "info"
    };
    // Another subscriber may be there already (the renderer's); then ours isn't needed.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
        .with_ansi(false)
        .with_writer(Scrubbing)
        .try_init()
        .ok();
}

struct Scrubbing;

impl<'a> MakeWriter<'a> for Scrubbing {
    type Writer = Line;

    fn make_writer(&'a self) -> Line {
        Line(Vec::new())
    }
}

/// One log line. `tracing` formats an event into one writer and drops it, so the line is whole
/// by then, and only then is it scrubbed and written.
struct Line(Vec<u8>);

impl Write for Line {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for Line {
    fn drop(&mut self) {
        let line = b33p_core::scrub::scrub(&String::from_utf8_lossy(&self.0));
        // Nowhere to report a failure of the log itself to.
        std::io::stderr().lock().write_all(line.as_bytes()).ok();
    }
}
