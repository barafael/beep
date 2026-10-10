//! When a conversation was active: its history counted per week or month, for overviews like
//! the activity strip beside a chat.

use jiff::{ToSpan, civil::Date, tz::TimeZone};

use crate::{AttachmentKind, ChatMessage};

/// How long a [`Bucket`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Span {
    Day,
    /// Starting on Monday.
    Week,
    Month,
}

impl Span {
    /// Weeks, unless that makes more buckets than an overview can show apart; then months.
    pub fn fitting(history: &[ChatMessage]) -> Self {
        let (Some(first), Some(last)) = (history.first(), history.last()) else {
            return Self::Week;
        };
        const MAX_WEEKS: u64 = 156;
        let week_ms = 7 * 24 * 60 * 60 * 1000;
        if last.timestamp.saturating_sub(first.timestamp) / week_ms > MAX_WEEKS {
            Self::Month
        } else {
            Self::Week
        }
    }

    fn start_of(self, date: Date) -> Date {
        match self {
            Self::Day => date,
            Self::Week => date
                .checked_sub(i64::from(date.weekday().to_monday_zero_offset()).days())
                .unwrap_or(date),
            Self::Month => date.first_of_month(),
        }
    }

    fn after(self, start: Date) -> Date {
        match self {
            Self::Day => start.saturating_add(1.day()),
            Self::Week => start.saturating_add(1.week()),
            Self::Month => start.saturating_add(1.month()),
        }
    }
}

/// One week or month of a conversation, in local time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bucket {
    /// Its first day.
    pub start: Date,
    /// Index of its first message in the conversation's history; of the next message if it has
    /// none.
    pub first: usize,
    /// Messages we sent.
    pub mine: u32,
    /// Messages everyone else sent.
    pub others: u32,
    /// Photos and videos.
    pub media: u32,
    /// Other files: documents, audio, voice messages.
    pub files: u32,
}

impl Bucket {
    fn new(start: Date, first: usize) -> Self {
        Self {
            start,
            first,
            mine: 0,
            others: 0,
            media: 0,
            files: 0,
        }
    }

    pub fn messages(&self) -> u32 {
        self.mine + self.others
    }

    /// `buckets` added up, starting where the first does.
    pub fn total(buckets: &[Bucket]) -> Bucket {
        let mut total = Bucket::new(buckets.first().map_or_else(Date::default, |b| b.start), 0);
        for bucket in buckets {
            total.mine += bucket.mine;
            total.others += bucket.others;
            total.media += bucket.media;
            total.files += bucket.files;
        }
        total
    }

    /// How many of its messages are ours, in percent.
    pub fn mine_percent(&self) -> u32 {
        (self.mine * 100).checked_div(self.messages()).unwrap_or(0)
    }

    /// "Monday, March 7, 2022", "Week of March 7, 2022" or "March 2022".
    pub fn label(&self, span: Span) -> String {
        match span {
            Span::Day => self.start.strftime("%A, %B %-d, %Y").to_string(),
            Span::Week => self.start.strftime("Week of %B %-d, %Y").to_string(),
            Span::Month => self.start.strftime("%B %Y").to_string(),
        }
    }

    /// "412 messages · 9 photos and videos · 2 files", or "No messages".
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.messages() > 0 {
            parts.push(count(self.messages(), "message", "messages"));
        }
        if self.media > 0 {
            parts.push(count(self.media, "photo or video", "photos and videos"));
        }
        if self.files > 0 {
            parts.push(count(self.files, "file", "files"));
        }
        if parts.is_empty() {
            "No messages".into()
        } else {
            parts.join(" · ")
        }
    }
}

fn count(n: u32, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// `history` (sorted, as [`crate::state::ChatState`] keeps it) counted per `span`, from the
/// first message's to the last message's, with the empty ones between.
pub fn buckets(history: &[ChatMessage], span: Span) -> Vec<Bucket> {
    let tz = TimeZone::system();
    let date_of = |timestamp: u64| {
        jiff::Timestamp::from_millisecond(timestamp as i64)
            .ok()
            .map(|t| t.to_zoned(tz.clone()).date())
    };
    // Where a bucket starting on `start` ends, in Signal's milliseconds.
    let end_of = |start: Date| {
        start
            .to_zoned(tz.clone())
            .map_or(u64::MAX, |z| z.timestamp().as_millisecond().max(0) as u64)
    };
    let Some(first_date) = history.first().and_then(|m| date_of(m.timestamp)) else {
        return Vec::new();
    };
    let mut buckets = Vec::new();
    let mut current = Bucket::new(span.start_of(first_date), 0);
    let mut end = end_of(span.after(current.start));
    for (index, message) in history.iter().enumerate() {
        while message.timestamp >= end {
            let next = span.after(current.start);
            if next <= current.start {
                break; // The end of time; count the rest here.
            }
            buckets.push(std::mem::replace(&mut current, Bucket::new(next, index)));
            end = end_of(span.after(next));
        }
        match (message.notice, message.outgoing) {
            // A notice (e.g. a new safety number) is nobody's message.
            (true, _) => {}
            (false, true) => current.mine += 1,
            (false, false) => current.others += 1,
        }
        for attachment in &message.attachments {
            match attachment.kind {
                AttachmentKind::Image | AttachmentKind::Video | AttachmentKind::ViewOnce => {
                    current.media += 1
                }
                AttachmentKind::File | AttachmentKind::Audio | AttachmentKind::VoiceNote => {
                    current.files += 1
                }
                AttachmentKind::Sticker | AttachmentKind::LongText => {}
            }
        }
    }
    buckets.push(current);
    buckets
}

/// A year of a conversation laid out like a wall calendar: a column per week, a row per
/// weekday (Monday first).
#[derive(Clone, Debug)]
pub struct CalendarYear<'a> {
    pub year: i16,
    /// Its days added up.
    pub total: Bucket,
    /// Every day of the year, January 1 first.
    pub days: Vec<CalendarDay<'a>>,
    /// The column each month begins in, January first.
    pub months: [u8; 12],
}

#[derive(Clone, Debug)]
pub struct CalendarDay<'a> {
    pub column: u8,
    pub row: u8,
    /// "Monday, March 7, 2022".
    pub label: String,
    /// What happened that day; `None` outside the conversation's history.
    pub bucket: Option<&'a Bucket>,
}

/// `days` (from [`buckets`] with [`Span::Day`]) by calendar year, oldest first.
pub fn calendar(days: &[Bucket]) -> Vec<CalendarYear<'_>> {
    let (Some(first), Some(last)) = (days.first(), days.last()) else {
        return Vec::new();
    };
    (first.start.year()..=last.start.year())
        .filter_map(|year| {
            let jan1 = Date::new(year, 1, 1).ok()?;
            let offset = jan1.weekday().to_monday_zero_offset() as i16;
            let column = |date: Date| ((date.day_of_year() - 1 + offset) / 7) as u8;
            let calendar_days: Vec<_> = jan1
                .series(1.day())
                .take_while(|date| date.year() == year)
                .map(|date| CalendarDay {
                    column: column(date),
                    row: date.weekday().to_monday_zero_offset() as u8,
                    label: date.strftime("%A, %B %-d, %Y").to_string(),
                    bucket: usize::try_from((date - first.start).get_days())
                        .ok()
                        .and_then(|i| days.get(i)),
                })
                .collect();
            let in_year: Vec<_> = calendar_days
                .iter()
                .filter_map(|d| d.bucket.cloned())
                .collect();
            let mut months = [0; 12];
            for (month, column_of) in (1..=12).zip(months.iter_mut()) {
                *column_of = column(Date::new(year, month, 1).ok()?);
            }
            Some(CalendarYear {
                year,
                total: Bucket::total(&in_year),
                days: calendar_days,
                months,
            })
        })
        .collect()
}

/// A stretch of an overview: a bucket, or a long silence folded into one entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stretch {
    Active(Bucket),
    Quiet {
        /// The first day without messages.
        from: Date,
        /// The day messages start again.
        until: Date,
        /// Index of the message after it.
        first: usize,
    },
}

impl Stretch {
    /// Index of its first message, or of the one after it.
    pub fn first(&self) -> usize {
        match self {
            Self::Active(bucket) => bucket.first,
            Self::Quiet { first, .. } => *first,
        }
    }

    /// "No messages for 14 months", "No messages for 9 weeks".
    pub fn quiet_summary(from: Date, until: Date) -> String {
        let span = from.until((jiff::Unit::Month, until)).unwrap_or_default();
        let months = span.get_months();
        let length = if months >= 2 {
            format!("{months} months")
        } else {
            let weeks = from.until(until).map_or(0, |s| s.get_days() / 7);
            format!("{weeks} weeks")
        };
        format!("No messages for {length}")
    }
}

/// `buckets` with every run of at least `min` empty ones folded into a [`Stretch::Quiet`].
pub fn fold_quiet(buckets: Vec<Bucket>, span: Span, min: usize) -> Vec<Stretch> {
    let mut stretches = Vec::new();
    let mut empty: Vec<Bucket> = Vec::new();
    let flush = |empty: &mut Vec<Bucket>, stretches: &mut Vec<Stretch>| {
        if empty.len() >= min.max(1) {
            let last = empty.last().expect("not empty");
            stretches.push(Stretch::Quiet {
                from: empty[0].start,
                until: span.after(last.start),
                first: empty[0].first,
            });
            empty.clear();
        } else {
            stretches.extend(empty.drain(..).map(Stretch::Active));
        }
    };
    for bucket in buckets {
        if bucket.messages() == 0 {
            empty.push(bucket);
        } else {
            flush(&mut empty, &mut stretches);
            stretches.push(Stretch::Active(bucket));
        }
    }
    flush(&mut empty, &mut stretches);
    stretches
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Attachment, AttachmentId};

    /// Noon local time on `date`, in Signal's milliseconds.
    fn at(date: Date) -> u64 {
        date.at(12, 0, 0, 0)
            .to_zoned(TimeZone::system())
            .unwrap()
            .timestamp()
            .as_millisecond() as u64
    }

    fn message(date: Date, outgoing: bool) -> ChatMessage {
        ChatMessage {
            timestamp: at(date),
            outgoing,
            ..Default::default()
        }
    }

    fn date(y: i16, m: i8, d: i8) -> Date {
        Date::new(y, m, d).unwrap()
    }

    #[test]
    fn notices_are_nobodys_messages() {
        let day = date(2026, 10, 10);
        let notice = ChatMessage {
            notice: true,
            ..message(day, false)
        };
        let history = [message(day, false), notice, message(day, true)];
        let buckets = buckets(&history, Span::Week);
        assert_eq!((buckets[0].others, buckets[0].mine), (1, 1));
    }

    #[test]
    fn counts_per_week_from_monday() {
        // 2024-03-04 is a Monday.
        let mut photo = message(date(2024, 3, 10), false);
        photo.attachments.push(Attachment {
            id: AttachmentId("p".into()),
            kind: AttachmentKind::Image,
            content_type: "image/jpeg".into(),
            file_name: None,
            size: 0,
            width: 0,
            height: 0,
            blurhash: None,
            caption: None,
            gif: false,
            pointer: Default::default(),
        });
        let history = vec![
            message(date(2024, 3, 5), true),
            photo,
            message(date(2024, 3, 11), false),
            message(date(2024, 3, 27), true),
        ];
        let buckets = buckets(&history, Span::Week);
        let summary: Vec<_> = buckets
            .iter()
            .map(|b| (b.start, b.first, b.mine, b.others, b.media))
            .collect();
        assert_eq!(
            summary,
            [
                (date(2024, 3, 4), 0, 1, 1, 1),
                (date(2024, 3, 11), 2, 0, 1, 0),
                (date(2024, 3, 18), 3, 0, 0, 0),
                (date(2024, 3, 25), 3, 1, 0, 0),
            ]
        );
        assert_eq!(buckets[0].summary(), "2 messages · 1 photo or video");
        assert_eq!(buckets[2].summary(), "No messages");
    }

    #[test]
    fn counts_per_month() {
        let history = vec![
            message(date(2023, 12, 31), false),
            message(date(2024, 2, 1), false),
        ];
        let starts: Vec<_> = buckets(&history, Span::Month)
            .iter()
            .map(|b| (b.start, b.messages()))
            .collect();
        assert_eq!(
            starts,
            [
                (date(2023, 12, 1), 1),
                (date(2024, 1, 1), 0),
                (date(2024, 2, 1), 1)
            ]
        );
        assert_eq!(
            buckets(&history, Span::Month)[0].label(Span::Month),
            "December 2023"
        );
    }

    #[test]
    fn folds_long_silences_only() {
        let history = vec![
            message(date(2022, 1, 15), true),
            message(date(2022, 3, 15), true),
            message(date(2023, 6, 15), false),
        ];
        let stretches = fold_quiet(buckets(&history, Span::Month), Span::Month, 3);
        let shape: Vec<_> = stretches
            .iter()
            .map(|s| match s {
                Stretch::Active(b) => format!("{} {}", b.start, b.messages()),
                Stretch::Quiet { from, until, first } => format!("quiet {from} {until} {first}"),
            })
            .collect();
        assert_eq!(
            shape,
            [
                "2022-01-01 1",
                "2022-02-01 0",
                "2022-03-01 1",
                "quiet 2022-04-01 2023-06-01 2",
                "2023-06-01 1",
            ]
        );
        assert_eq!(
            Stretch::quiet_summary(date(2022, 4, 1), date(2023, 6, 1)),
            "No messages for 14 months"
        );
    }

    #[test]
    fn counts_per_day() {
        let history = vec![
            message(date(2024, 2, 28), true),
            message(date(2024, 3, 1), false),
            message(date(2024, 3, 1), true),
        ];
        let days: Vec<_> = buckets(&history, Span::Day)
            .iter()
            .map(|b| (b.start, b.first, b.messages()))
            .collect();
        assert_eq!(
            days,
            [
                (date(2024, 2, 28), 0, 1),
                (date(2024, 2, 29), 1, 0),
                (date(2024, 3, 1), 1, 2)
            ]
        );
        assert_eq!(
            buckets(&history, Span::Day)[2].label(Span::Day),
            "Friday, March 1, 2024"
        );
    }

    #[test]
    fn lays_out_years_like_a_calendar() {
        let history = vec![
            message(date(2023, 12, 31), true),
            message(date(2024, 1, 1), false),
        ];
        let days = buckets(&history, Span::Day);
        let years = calendar(&days);
        assert_eq!(years.len(), 2);
        // 2023-12-31 is a Sunday: the last row, in the 53rd column.
        let dec31 = years[0].days.last().unwrap();
        assert_eq!((dec31.column, dec31.row), (52, 6));
        assert_eq!(dec31.bucket.map(Bucket::messages), Some(1));
        assert!(years[0].days[0].bucket.is_none());
        // 2024-01-01 is a Monday: top left.
        let jan1 = &years[1].days[0];
        assert_eq!((jan1.column, jan1.row), (0, 0));
        assert_eq!(jan1.label, "Monday, January 1, 2024");
        assert_eq!(years[1].days.len(), 366);
        assert_eq!(years[1].total.messages(), 1);
        assert_eq!(years[1].months[2], 8); // March 1 is in the ninth week.
        assert_eq!(years[0].total.mine_percent(), 100);
    }

    #[test]
    fn picks_months_for_long_histories() {
        let short = [
            message(date(2022, 1, 1), true),
            message(date(2024, 1, 1), true),
        ];
        let long = [
            message(date(2019, 1, 1), true),
            message(date(2024, 1, 1), true),
        ];
        assert_eq!(Span::fitting(&short), Span::Week);
        assert_eq!(Span::fitting(&long), Span::Month);
        assert!(buckets(&[], Span::Week).is_empty());
    }
}
