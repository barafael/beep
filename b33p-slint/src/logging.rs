//! The log filter. presage and libsignal-service log whole messages at `debug` in a few places
//! (`?msg` on typing, story and edit messages), and a message's `Debug` prints its text, so
//! those crates are held at `info` whatever `RUST_LOG` says. `B33P_LOG_PLAINTEXT=1` lifts
//! that, for debugging the protocol on purpose.
//!
//! Ported from fix/security-review (f3a2f15), with that opt-out added.

/// Targets that can hold message contents in memory, and so must never log at `debug`.
///
/// `Content` derives `Debug`, and that prints the message body in plaintext, so a single
/// `debug!(?msg, …)` anywhere in these crates writes the conversation to the log.
const PLAINTEXT_TARGETS: [&str; 2] = ["presage", "libsignal_service"];

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

/// The filter for this process: [`log_filter`] unless plaintext logging was asked for.
pub fn from_env() -> tracing_subscriber::EnvFilter {
    let rust_log = std::env::var("RUST_LOG").unwrap_or_default();
    if std::env::var_os("B33P_LOG_PLAINTEXT").is_some() {
        return tracing_subscriber::EnvFilter::builder()
            .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
            .parse_lossy(rust_log);
    }
    log_filter(&rust_log)
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

    /// `info` still works, so this isn't just "silence everything".
    #[test]
    fn info_is_still_logged() {
        let info = || tracing::info!(target: "presage::manager::registered", answer = 42, "hello");
        assert!(logged("", info).contains("42"));
    }
}
