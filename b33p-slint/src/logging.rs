//! The log: its filter, and where it goes.
//!
//! presage and libsignal-service log whole messages at `debug` in a few places (`?msg` on
//! typing, story and edit messages), and a message's `Debug` prints its text, so those crates
//! are held at `info` whatever `RUST_LOG` says. What is logged at `info` still names people
//! (service IDs, protocol addresses, numbers), in b33p's lines and the libraries' alike, so
//! every line is scrubbed ([`b33p_core::scrub`]) on its way out. The log goes to stderr and to a
//! file ([`b33p_core::settings::log_dir`]), which is what a bug report can carry.
//! `B33P_LOG_PLAINTEXT=1` lifts the cap and the scrubbing, and keeps the log off the disk, for
//! debugging the protocol on purpose.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tracing_subscriber::fmt::MakeWriter;

/// Targets that can hold message contents in memory, and so must never log at `debug`.
///
/// `Content` derives `Debug`, and that prints the message body in plaintext, so a single
/// `debug!(?msg, …)` anywhere in these crates writes the conversation to the log.
const PLAINTEXT_TARGETS: [&str; 3] = ["presage", "libsignal_service", "libsignal_protocol"];

/// Builds the log filter from `RUST_LOG`.
///
/// `RUST_LOG` still raises verbosity, but it cannot lift [`PLAINTEXT_TARGETS`] out of `info`.
/// There is no way to redact this at the call site: by the time a value reaches a log statement
/// it has already been formatted, so the filter has to be the thing that decides. Debugging
/// b33p itself stays possible, since `b33p_core` is not capped.
pub fn log_filter(rust_log: &str) -> tracing_subscriber::EnvFilter {
    use tracing::level_filters::LevelFilter;

    let mut filter = tracing_subscriber::EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .parse_lossy(clamp_plaintext(rust_log));
    for target in PLAINTEXT_TARGETS {
        filter = filter.add_directive(
            format!("{target}=info")
                .parse()
                .expect("a target and a level are a valid directive"),
        );
    }
    filter
}

/// Rewrites `rust_log` so that no directive in it can enable `debug` or `trace` for a
/// [`PLAINTEXT_TARGETS`] crate.
///
/// The broad cap added in [`log_filter`] is not enough on its own: `EnvFilter` picks the *most
/// specific* matching directive, so `RUST_LOG=presage::manager::registered=trace` beats
/// `presage=info`. Rewriting the level is the only way to make the cap stick, and doing it on the
/// string keeps the decision visible rather than relying on precedence rules. Regex directives are
/// dropped, since they can name any target at all.
fn clamp_plaintext(rust_log: &str) -> String {
    use tracing::level_filters::LevelFilter;

    let mut kept: Vec<String> = Vec::new();
    for directive in rust_log.split(',').map(str::trim).filter(|d| !d.is_empty()) {
        // A regex directive such as `/registered/=trace` can match anything, so there is no
        // telling what it would enable.
        if directive.starts_with('/') {
            continue;
        }
        // The level follows the last `=`, which comes after any span filter: a field
        // comparison such as `target[span{peer=x}]` has an `=` of its own.
        let after_span = directive.rfind(']').map_or(0, |i| i + 1);
        let level_at = directive[after_span..].find('=').map(|i| after_span + i);
        let (target, below_info) = match level_at {
            Some(at) => (
                &directive[..at],
                directive[at + 1..]
                    .parse::<LevelFilter>()
                    .is_ok_and(|level| level > LevelFilter::INFO),
            ),
            // A bare level such as `debug` applies to everything, but the caps are per-crate,
            // so they are the more specific match and win on their own.
            None if directive.parse::<LevelFilter>().is_ok() => {
                kept.push(directive.to_string());
                continue;
            }
            // Anything else without a level is a target, enabled at `trace`.
            None => (directive, true),
        };
        // `target[span{field}]=level` is valid too; compare the module path on its own. A
        // directive's target matches every target it begins, so `presag` or `presage:` reach
        // into presage as much as `presage::manager` does.
        let module = target.split_once('[').map_or(target, |(module, _)| module);
        let plaintext = PLAINTEXT_TARGETS
            .iter()
            .any(|name| name.starts_with(module) || module.starts_with(name));
        kept.push(match (plaintext, below_info) {
            (true, true) => format!("{target}=info"),
            _ => directive.to_string(),
        });
    }
    kept.join(",")
}

/// Whether the log may hold message contents and names: `B33P_LOG_PLAINTEXT`.
fn plaintext() -> bool {
    std::env::var_os("B33P_LOG_PLAINTEXT").is_some()
}

/// The filter for this process: [`log_filter`] unless plaintext logging was asked for.
pub fn from_env() -> tracing_subscriber::EnvFilter {
    let rust_log = std::env::var("RUST_LOG").unwrap_or_default();
    if plaintext() {
        return tracing_subscriber::EnvFilter::builder()
            .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
            .parse_lossy(rust_log);
    }
    log_filter(&rust_log)
}

/// Starts logging for this process: to stderr and, scrubbed, to the log file.
pub fn init() {
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    let registry = tracing_subscriber::registry().with(from_env());
    if plaintext() {
        registry
            .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
            .init();
        return;
    }
    let file = b33p_core::settings::log_dir().and_then(|dir| match LogFile::open(&dir) {
        Ok(file) => Some(file),
        Err(error) => {
            eprintln!("b33p: no log file in {}: {error}", dir.display());
            None
        }
    });
    registry
        .with(tracing_subscriber::fmt::layer().with_writer(Scrubbing(std::io::stderr)))
        .with(file.map(|file| {
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(Scrubbing(file))
        }))
        .init();
}

/// Writes each log line scrubbed of user data. tracing formats an event into one writer and
/// then drops it, so the line is whole by then.
struct Scrubbing<M>(M);

impl<'a, M: MakeWriter<'a>> MakeWriter<'a> for Scrubbing<M> {
    type Writer = ScrubbingWriter<M::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        ScrubbingWriter {
            inner: self.0.make_writer(),
            line: Vec::new(),
        }
    }
}

struct ScrubbingWriter<W: Write> {
    inner: W,
    line: Vec<u8>,
}

impl<W: Write> Write for ScrubbingWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.line.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.line.is_empty() {
            return Ok(());
        }
        let line = b33p_core::scrub::scrub(&String::from_utf8_lossy(&self.line));
        self.line.clear();
        self.inner.write_all(line.as_bytes())?;
        self.inner.flush()
    }
}

impl<W: Write> Drop for ScrubbingWriter<W> {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

/// The log file, `b33p.log`, private to its owner. Past [`LogFile::LIMIT`] it becomes
/// `b33p.log.1` (replacing the one before) and a new one starts, so the two hold the recent
/// past and never grow without end.
struct LogFile {
    path: PathBuf,
    limit: u64,
    /// The open file and how much it holds.
    file: Mutex<(File, u64)>,
}

impl LogFile {
    const LIMIT: u64 = 5 * 1024 * 1024;

    fn open(dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let path = dir.join("b33p.log");
        let file = Self::append(&path)?;
        let size = file.metadata()?.len();
        Ok(Self {
            path,
            limit: Self::LIMIT,
            file: Mutex::new((file, size)),
        })
    }

    fn append(path: &Path) -> std::io::Result<File> {
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options.open(path)
    }

    fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
        let mut file = self
            .file
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if file.1 > 0 && file.1 + bytes.len() as u64 > self.limit {
            std::fs::rename(&self.path, self.path.with_extension("log.1"))?;
            *file = (Self::append(&self.path)?, 0);
        }
        file.0.write_all(bytes)?;
        file.1 += bytes.len() as u64;
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for LogFile {
    type Writer = &'a LogFile;

    fn make_writer(&'a self) -> Self::Writer {
        self
    }
}

impl Write for &LogFile {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        LogFile::write(self, bytes)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{PLAINTEXT_TARGETS, log_filter};
    use std::sync::{Arc, Mutex};

    /// Captures formatted log output so a test can assert on what was written.
    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Sink {
        type Writer = Sink;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// Runs `emit` under the filter `log_filter` builds from `rust_log`, and returns what the
    /// subscriber wrote.
    fn logged(rust_log: &str, emit: impl FnOnce()) -> String {
        let sink = Sink::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(sink.clone())
            .with_env_filter(log_filter(rust_log))
            .finish();
        tracing::subscriber::with_default(subscriber, emit);
        String::from_utf8(sink.0.lock().unwrap().clone()).unwrap()
    }

    /// The site in presage that logs a whole `Content`, whose derived `Debug` prints the
    /// message body in plaintext.
    fn presage_debug() {
        tracing::debug!(target: "presage::manager::registered", answer = 42, "a message body");
    }

    #[test]
    fn rust_log_cannot_turn_on_presage_debug_logging() {
        for rust_log in [
            "debug",
            "trace",
            "presage=debug",
            "presage::manager::registered=trace",
            // A target without a level means `trace`.
            "presage",
            "presage::manager::registered",
            "presage::manager::registered[receive]",
            "presage::manager::registered[receive{peer=x}]",
            "presage::manager::registered[receive{peer=x}]=trace",
            "info,presage::manager::registered",
            // Targets match by their beginning.
            "presage:=trace",
            "presag=debug",
            "pre=trace",
            // Regex directives match anything, so they are dropped outright.
            "/registered/=trace",
            "/^presage.*/=debug",
        ] {
            let logged = logged(rust_log, presage_debug);
            assert!(
                !logged.contains("42"),
                "`{rust_log}` logged a body: {logged}"
            );
        }
    }

    /// The other crate that can hold message contents is capped too, not only the one with the
    /// logging site found today: a future logging call must not undo this.
    #[test]
    fn every_plaintext_target_is_capped() {
        for target in PLAINTEXT_TARGETS {
            for rust_log in ["debug", "trace", &format!("{target}=debug")] {
                let logged = logged(rust_log, || {
                    // One callsite per capped crate, since the target must be a literal.
                    match target {
                        "presage" => tracing::debug!(
                            target: "presage::manager::registered",
                            answer = 42,
                            "a message body"
                        ),
                        "libsignal_protocol" => tracing::debug!(
                            target: "libsignal_protocol::session_cipher",
                            answer = 42,
                            "a message body"
                        ),
                        _ => tracing::debug!(
                            target: "libsignal_service::cipher::decrypt",
                            answer = 42,
                            "a message body"
                        ),
                    }
                });
                assert!(
                    !logged.contains("42"),
                    "`{rust_log}` logged from {target}: {logged}"
                );
            }
        }
    }

    /// Debugging b33p itself must keep working, or nobody will ever use `RUST_LOG` again.
    #[test]
    fn rust_log_still_debugs_b33p() {
        let debug = || tracing::debug!(target: "b33p_core::signal", answer = 42, "a hint");
        assert!(logged("debug", debug).contains("42"));
        assert!(logged("b33p_core=debug", debug).contains("42"));
        assert!(logged("b33p_core::signal", debug).contains("42"));
        // Nothing is logged at debug when `RUST_LOG` is unset.
        assert!(!logged("", debug).contains("42"));
    }

    /// What the libraries log at `info` names people; none of it reaches the log.
    #[test]
    fn every_line_is_scrubbed() {
        let sink = Sink::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(super::Scrubbing(sink.clone()))
            .with_env_filter(log_filter(""))
            .finish();
        let aci = "8c2a6a3e-0a52-4b3c-9d1e-0123456789ab";
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "presage::manager::registered", sender_uuid = aci, "got a message");
            tracing::info!(target: "libsignal_protocol::session", "{aci}.2 creating new chains");
            tracing::info!(target: "b33p_core::signal", sender = %format!("PNI:{aci}"), "reset");
            tracing::info!(target: "b33p_core::signal", "found +49 151 2345 6789");
        });
        let logged = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
        assert_eq!(logged.lines().count(), 4, "{logged}");
        assert!(!logged.contains("8c2a6a3e"), "{logged}");
        assert!(!logged.contains("2345"), "{logged}");
        assert!(logged.contains("creating new chains"));
    }

    /// The log file stays under its limit, with one older file beside it.
    #[test]
    fn the_log_file_rotates() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().unwrap();
        let mut log = super::LogFile::open(dir.path()).unwrap();
        log.limit = 100;
        for n in 0..30 {
            writeln!(&log, "line {n:03} of the log").unwrap();
        }
        let current = std::fs::read_to_string(dir.path().join("b33p.log")).unwrap();
        let older = std::fs::read_to_string(dir.path().join("b33p.log.1")).unwrap();
        assert!(current.len() <= 100 && older.len() <= 100);
        assert!(current.ends_with("line 029 of the log\n"));
        assert!(
            older
                .lines()
                .last()
                .is_some_and(|line| current.lines().next().unwrap() > line)
        );
    }

    /// `info` still works, so this isn't just "silence everything".
    #[test]
    fn info_is_still_logged() {
        let info = || tracing::info!(target: "presage::manager::registered", answer = 42, "hello");
        assert!(logged("", info).contains("42"));
    }
}
