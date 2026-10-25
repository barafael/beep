//! Time: the state's clock, and times and days as the window shows them.

use super::*;

/// Where [`ChatState`] takes the time from: the system's clock, or one set by hand, so that
/// tests of what depends on the time (typing running out, the window for deleting for
/// everyone, mutes) needn't sleep or race the clock.
#[derive(Clone, Debug, Default)]
pub struct Clock(Option<Arc<std::sync::atomic::AtomicU64>>);

impl Clock {
    /// A clock that stays at `at` (ms since the epoch) until set.
    pub fn fixed(at: u64) -> Self {
        Self(Some(Arc::new(at.into())))
    }

    /// Moves a [`Clock::fixed`] clock; the system's can't be.
    pub fn set(&self, at: u64) {
        if let Some(time) = &self.0 {
            time.store(at, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Milliseconds since the epoch.
    pub fn now(&self) -> u64 {
        match &self.0 {
            Some(time) => time.load(std::sync::atomic::Ordering::Relaxed),
            None => now_millis(),
        }
    }
}

/// How long until `at` (ms since the epoch), roughly: "5m", "3h", "2d".
pub fn format_remaining(at: u64, now: u64) -> String {
    let seconds = at.saturating_sub(now).div_ceil(1000);
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds.div_ceil(60)),
        3600..86400 => format!("{}h", seconds.div_ceil(3600)),
        _ => format!("{}d", seconds.div_ceil(86400)),
    }
}

/// Signal's milliseconds as local time.
pub(super) fn zoned(timestamp: u64) -> Option<jiff::Zoned> {
    let time = jiff::Timestamp::from_millisecond(timestamp as i64).ok()?;
    Some(time.to_zoned(jiff::tz::TimeZone::system()))
}

/// The day of Signal's milliseconds in `tz`: looked up once, for many messages.
pub(crate) fn local_date_in(tz: &jiff::tz::TimeZone, timestamp: u64) -> Option<jiff::civil::Date> {
    let time = jiff::Timestamp::from_millisecond(timestamp as i64).ok()?;
    Some(tz.to_datetime(time).date())
}

/// "14:05": the time of day, for message bubbles (days have their own separators).
pub fn format_clock(timestamp: u64) -> String {
    zoned(timestamp).map_or_else(String::new, |time| time.strftime("%H:%M").to_string())
}

/// For the chat list: "14:05" today, "Mon" this week, "Oct 2" this year, "Oct 2, 2024" before.
pub fn format_list_time(timestamp: u64) -> String {
    format_list_time_at(timestamp, &jiff::Zoned::now())
}

/// [`format_list_time`] for many timestamps, e.g. a whole chat list's: the present is looked
/// up once.
pub fn list_time_formatter() -> impl Fn(u64) -> String {
    let now = jiff::Zoned::now();
    move |timestamp| format_list_time_at(timestamp, &now)
}

/// How long until the next local midnight, when what [`format_list_time`] says changes: today's
/// "14:05" becomes "Mon", and so on.
pub fn until_midnight() -> std::time::Duration {
    until_midnight_after(&jiff::Zoned::now())
}

pub(super) fn until_midnight_after(now: &jiff::Zoned) -> std::time::Duration {
    now.tomorrow()
        .and_then(|tomorrow| tomorrow.start_of_day())
        .ok()
        .and_then(|midnight| now.duration_until(&midnight).try_into().ok())
        // Only at the end of time.
        .unwrap_or(std::time::Duration::from_secs(3600))
}

pub(super) fn format_list_time_at(timestamp: u64, now: &jiff::Zoned) -> String {
    let Some(time) = zoned(timestamp) else {
        return String::new();
    };
    let days = (now.date() - time.date()).get_days();
    let format = match days {
        0 => "%H:%M",
        1..=6 => "%a",
        _ if time.year() == now.year() => "%b %-d",
        _ => "%b %-d, %Y",
    };
    time.strftime(format).to_string()
}

/// For day separators: "Today", "Yesterday", "Monday", "Oct 2", "Oct 2, 2024".
pub(super) fn day_label(timestamp: u64, now: &jiff::Zoned) -> String {
    let Some(time) = zoned(timestamp) else {
        return String::new();
    };
    match (now.date() - time.date()).get_days() {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        2..=6 => time.strftime("%A").to_string(),
        _ if time.year() == now.year() => time.strftime("%B %-d").to_string(),
        _ => time.strftime("%B %-d, %Y").to_string(),
    }
}

/// "14:05" for today, "Mar  3, 14:05" otherwise, in local time.
pub fn format_time(timestamp: u64) -> String {
    format_time_on(timestamp, jiff::Zoned::now().date())
}

/// [`format_time`] for many timestamps, e.g. the media overview's: today is looked up once.
pub fn time_formatter() -> impl Fn(u64) -> String {
    let today = jiff::Zoned::now().date();
    move |timestamp| format_time_on(timestamp, today)
}

pub(super) fn format_time_on(timestamp: u64, today: jiff::civil::Date) -> String {
    let Some(time) = zoned(timestamp) else {
        return String::new();
    };
    let format = if time.date() == today {
        "%H:%M"
    } else {
        "%b %e, %H:%M"
    };
    time.strftime(format).to_string()
}
