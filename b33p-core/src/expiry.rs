//! A build of b33p expires, as Signal's apps do: Signal changes its servers behind its clients,
//! and a client months old may no longer speak to them as they expect. Signal-Desktop expires
//! 90 days after its build (scripts/get-expire-time.mjs; ts/util/buildExpiration.std.ts), at
//! once when the server answers 499 (the client is too old), and stops talking to Signal then:
//! nothing is received or sent, what is here can still be read.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// How long a build is good for after it was made (Signal-Desktop's, for a build that updates).
pub(crate) const VALID_FOR: Duration = Duration::from_secs(90 * 24 * 60 * 60);

/// When this build was made, if it is known (`build.rs`).
pub(crate) const BUILD_TIME: Option<&str> = option_env!("B33P_BUILD_TIME");

/// When a build made at `built` expires.
pub(crate) fn expires_at(built: SystemTime) -> SystemTime {
    built + VALID_FOR
}

/// Whether a build made at `built` has expired by `now`. Also when its expiry lies further
/// ahead than it could (the clock is wrong), as Signal-Desktop takes it.
pub(crate) fn has_expired(built: SystemTime, now: SystemTime) -> bool {
    let expires = expires_at(built);
    expires <= now || expires > now + VALID_FOR + DAY
}

/// When this build expires: only a release build made from a known commit does. A build for
/// working on b33p (debug, or without its build time) never does, as Signal-Desktop's builds
/// outside a package don't.
pub(crate) fn this_build_expires() -> Option<SystemTime> {
    if cfg!(debug_assertions) {
        return None;
    }
    let seconds: u64 = BUILD_TIME?.parse().ok()?;
    Some(expires_at(UNIX_EPOCH + Duration::from_secs(seconds)))
}

/// Whether this build has expired by `now` ([`this_build_expires`]).
pub(crate) fn this_build_has_expired(now: SystemTime) -> bool {
    if cfg!(debug_assertions) {
        return false;
    }
    BUILD_TIME
        .and_then(|time| time.parse().ok())
        .is_some_and(|seconds| has_expired(UNIX_EPOCH + Duration::from_secs(seconds), now))
}

/// What names this build, for remembering that the server called it too old: its build time,
/// else its version. A new build is not too old until the server says so of it.
pub(crate) fn this_build() -> i64 {
    BUILD_TIME
        .and_then(|time| time.parse().ok())
        .unwrap_or_else(|| {
            crate::version::CURRENT.bytes().fold(0i64, |hash, byte| {
                hash.wrapping_mul(31).wrapping_add(i64::from(byte))
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_expires_ninety_days_after_it_was_made() {
        let built = UNIX_EPOCH + Duration::from_secs(1_790_000_000);
        assert!(!has_expired(built, built + 89 * DAY));
        assert!(has_expired(built, built + 90 * DAY));
    }

    #[test]
    fn an_expiry_too_far_ahead_means_the_clock_is_wrong() {
        let built = UNIX_EPOCH + Duration::from_secs(1_790_000_000);
        // The clock says it is long before the build was made.
        assert!(has_expired(built, built - 30 * DAY));
        assert!(!has_expired(built, built - DAY / 2));
    }
}
