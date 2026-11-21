//! The open chat's messages as rows: who wrote them in a run, days, what's new.

use super::*;

/// What the rows of [`ChatState::messages`] have in common, worked out once per call. A row
/// depends on no more than its neighbours, so any one can be worked out alone.
pub(super) struct Rows<'a> {
    pub(super) state: &'a ChatState,
    pub(super) history: &'a [ChatMessage],
    pub(super) group: bool,
    pub(super) now: jiff::ZonedDateTime,
    /// `now` in ms since the epoch.
    pub(super) now_ms: u64,
    pub(super) tz: jiff::tz::TimeZone,
    pub(super) first_unread: Option<usize>,
    /// In a group, who read up to which message of ours (by its time), as far as known.
    pub(super) readers: HashMap<u64, Vec<&'a Person>>,
}

/// A message's local date, if its timestamp has one; `None` past either end of the history.
pub(super) type RowDate = Option<Option<jiff::civil::Date>>;

impl<'a> Rows<'a> {
    pub(super) fn new(state: &'a ChatState) -> Self {
        let history = state.selected_history();
        // Not in chats never read here: everything would be "new".
        let read_mark = state.read_mark.filter(|&mark| mark > 0);
        let tz = jiff::tz::TimeZone::system();
        let now = jiff::Timestamp::from_millisecond(state.now() as i64)
            .unwrap_or_else(|_| jiff::Timestamp::now());
        Self {
            state,
            history,
            group: state.selected().is_some_and(Conversation::is_group),
            now: now.to_zoned_datetime(tz.clone()),
            now_ms: u64::try_from(now.as_millisecond()).unwrap_or(0),
            tz,
            first_unread: read_mark.and_then(|mark| first_unread(history, mark)),
            readers: readers(state),
        }
    }

    pub(super) fn date(&self, i: usize) -> RowDate {
        let message = self.history.get(i)?;
        Some(local_date_in(&self.tz, message.timestamp))
    }

    pub(super) fn view(&self, i: usize) -> MessageView<'a> {
        let before = i.checked_sub(1).and_then(|i| self.date(i));
        self.view_dated(i, [before, self.date(i), self.date(i + 1)])
    }

    /// The `i`th row, given the dates of the message before it, its own, and the one after.
    pub(super) fn view_dated(
        &self,
        i: usize,
        [before, date, after]: [RowDate; 3],
    ) -> MessageView<'a> {
        let (state, history) = (self.state, self.history);
        let message = &history[i];
        let previous = i.checked_sub(1).map(|i| &history[i]);
        let next = history.get(i + 1);
        let first_of_run = previous.is_none_or(|p| !same_run(p, message, before == date));
        let full_text = state.full_text(message);
        MessageView {
            message,
            show_sender: self.group && !message.outgoing && first_of_run,
            body: match (message.deleted, full_text) {
                (true, _) => DELETED,
                (false, Some(text)) => text,
                (false, None) => &message.body,
            },
            // A fetched long text has no formatting of its own.
            styles: if message.deleted || full_text.is_some() {
                &[]
            } else {
                &message.styles
            },
            mentions: if message.deleted || full_text.is_some() {
                &[]
            } else {
                &message.mentions
            },
            day: (before != date).then(|| day_label(message.timestamp, &self.now)),
            time: format_clock_at(message.timestamp, self.now_ms),
            first_of_run,
            last_of_run: next.is_none_or(|n| !same_run(message, n, date == after)),
            show_time: next.is_none_or(|n| {
                !same_run(message, n, date == after)
                    || n.timestamp / 60_000 != message.timestamp / 60_000
            }),
            first_unread: self.first_unread == Some(i),
            revealed: state.revealed.contains(message_key(message)),
            highlighted: state.highlight.as_ref().is_some_and(|h| message.is(h)),
            selected: state.selection.contains(message_key(message)),
            readers: if message.outgoing {
                self.readers
                    .get(&message.timestamp)
                    .cloned()
                    .unwrap_or_default()
            } else {
                Vec::new()
            },
            sender_avatar: if self.group && !message.outgoing {
                state.people_avatars.get(&message.sender_id)
            } else {
                None
            },
        }
    }
}

/// Rows are worked out as they are reached, so skipping some (`nth`, `skip`) costs nothing.
pub(super) struct Messages<'a> {
    pub(super) rows: Rows<'a>,
    pub(super) at: Range<usize>,
    /// The row last reached, with its date and the next message's: going on from it, only
    /// the date of the message after the next is new.
    pub(super) dated: Option<(usize, [RowDate; 2])>,
}

impl<'a> Messages<'a> {
    pub(super) fn new(rows: Rows<'a>, at: Range<usize>) -> Self {
        Self {
            rows,
            at,
            dated: None,
        }
    }
}

impl<'a> Iterator for Messages<'a> {
    type Item = MessageView<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let i = self.at.next()?;
        let dates = match self.dated {
            Some((last, [date, after])) if last + 1 == i => [date, after, self.rows.date(i + 1)],
            _ => {
                let before = i.checked_sub(1).and_then(|i| self.rows.date(i));
                [before, self.rows.date(i), self.rows.date(i + 1)]
            }
        };
        self.dated = Some((i, [dates[1], dates[2]]));
        Some(self.rows.view_dated(i, dates))
    }

    fn nth(&mut self, n: usize) -> Option<Self::Item> {
        if n > 0 {
            self.at.nth(n - 1)?;
        }
        self.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.at.size_hint()
    }

    fn count(self) -> usize {
        self.at.len()
    }

    fn last(mut self) -> Option<Self::Item> {
        self.at.next_back().map(|i| self.rows.view(i))
    }
}

impl ExactSizeIterator for Messages<'_> {}

/// The open group's members by the time of the newest message of ours each read; empty for a
/// chat of two, where the ticks say it.
fn readers(state: &ChatState) -> HashMap<u64, Vec<&Person>> {
    let mut readers: HashMap<u64, Vec<&Person>> = HashMap::new();
    let Some(group) = state.selected().filter(|c| c.is_group()) else {
        return readers;
    };
    let Some(newest) = state.readers.get(&group.id) else {
        return readers;
    };
    for member in &group.members {
        if let Some(&read) = newest.get(&member.service_id) {
            readers.entry(read).or_default().push(member);
        }
    }
    readers
}

/// "Read by Ann", "Read by Ann and Bob", "Read by Ann, Bob and Cid", "Read by Ann, Bob and 2
/// others": for a message's readers, as a screen reader says them.
pub fn readers_label(readers: &[&Person]) -> String {
    let names: Vec<&str> = readers.iter().map(|p| p.name.as_str()).collect();
    match names.as_slice() {
        [] => String::new(),
        [one] => format!("Read by {one}"),
        [a, b] => format!("Read by {a} and {b}"),
        [a, b, c] => format!("Read by {a}, {b} and {c}"),
        [a, b, rest @ ..] => format!("Read by {a}, {b} and {} others", rest.len()),
    }
}

/// Messages by the same person within a few minutes of each other, on the same day.
pub(super) fn same_run(a: &ChatMessage, b: &ChatMessage, same_day: bool) -> bool {
    !a.notice
        && !b.notice
        && a.sender_id == b.sender_id
        && b.timestamp.saturating_sub(a.timestamp) <= RUN_GAP_MS
        && same_day
}

/// Where the first incoming message after `last_read` is: the history is sorted by time.
pub(super) fn first_unread(history: &[ChatMessage], last_read: u64) -> Option<usize> {
    let after = history.partition_point(|m| m.timestamp <= last_read);
    let first = history[after..]
        .iter()
        .position(|m| !m.outgoing && !m.notice)?;
    Some(after + first)
}
