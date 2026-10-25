use super::*;
use proptest::prelude::*;

fn piece() -> impl Strategy<Value = String> {
    prop_oneof![
        6 => prop::sample::select(vec![
            "a", "B", "c", "x", " ", "  ", "\n", "\t", "é", "É", "🦀", "e\u{0301}", "İ", "ı", "ẞ", "ß",
            "Ω", "ω", "\u{212A}", "k", "K", "Σ", "σ", "ς", "\u{00A0}", "\u{3000}", "ǅ", "ﬃ", "ŉ",
            "Ⅻ", "ǰ", "ΐ", "@", ".", "*", "<", "#",
        ]).prop_map(str::to_string),
        1 => any::<char>().prop_map(|c| c.to_string()),
    ]
}

fn text() -> impl Strategy<Value = String> {
    prop::collection::vec(piece(), 0..40).prop_map(|v| v.concat())
}

/// Lowercase by char, with both sigmas the same (snippet compares per char).
fn fold(c: char) -> String {
    let s: String = c.to_lowercase().collect();
    if s == "ς" { "σ".into() } else { s }
}

/// Inserts in order, or replaces the message if it is already there; returns whether it was
/// new. How pages of history were merged, one message after the other.
fn upsert(history: &mut Vec<ChatMessage>, message: ChatMessage) -> bool {
    match history.binary_search_by(|m| message_key(m).cmp(&message_key(&message))) {
        Ok(pos) => {
            history[pos] = message;
            false
        }
        Err(pos) => {
            history.insert(pos, message);
            true
        }
    }
}

fn page_message((timestamp, sender): (u64, usize), body: String) -> ChatMessage {
    ChatMessage {
        sender_id: ["ann", "bob", "cy"][sender].into(),
        timestamp,
        body,
        ..ChatMessage::default()
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 5000, failure_persistence: None, ..ProptestConfig::default() })]

    /// A page merged at once leaves the history, and counts, as upserting its messages one
    /// by one did: older pages, newer ones, overlapping, unsorted, with repeats.
    #[test]
    fn pages_merge_as_one_by_one(
        history in prop::collection::btree_set((0u64..40, 0usize..3), 0..12),
        page in prop::collection::vec((0u64..40, 0usize..3), 0..12),
        older in any::<bool>(),
    ) {
        let shift = if older { 0 } else { 20 };
        let history: Vec<ChatMessage> = history
            .into_iter()
            .map(|(t, s)| page_message((t + 20, s), format!("old {t}")))
            .collect();
        let page: Vec<ChatMessage> = page
            .into_iter()
            .enumerate()
            .map(|(i, (t, s))| page_message((t / 2 + shift, s), format!("new {i}")))
            .collect();
        let mut one_by_one = history.clone();
        let (mut added, mut replaced) = (0, 0);
        for message in page.clone() {
            if upsert(&mut one_by_one, message) {
                added += 1;
            } else {
                replaced += 1;
            }
        }
        let mut at_once = history;
        let counts = upsert_all(&mut at_once, page);
        prop_assert_eq!(counts, (added, replaced));
        prop_assert_eq!(at_once, one_by_one);
    }

    /// Lookups that halve the history find what going through it finds, also among several
    /// messages at the same moment (ours while sending have no sender ID yet).
    #[test]
    fn halving_finds_what_searching_finds(
        keys in prop::collection::btree_set((0u64..10, 0usize..4), 0..20),
        t in 0u64..11,
        sender in 0usize..4,
    ) {
        const SENDERS: [&str; 4] = ["ann", "bob", "cy", ""];
        let mut history: Vec<ChatMessage> = keys
            .into_iter()
            .map(|(t, s)| ChatMessage {
                sender_id: SENDERS[s].into(),
                timestamp: t,
                unsent: (s == 3).then_some(Unsent::Sending),
                ..ChatMessage::default()
            })
            .collect();
        history.sort_by(|a, b| message_key(a).cmp(&message_key(b)));
        let id = SENDERS[sender];
        prop_assert_eq!(
            find(&history, (t, id)),
            history.iter().position(|m| m.timestamp == t && m.sender_id == id)
        );
        let sent = at_time(&history, t);
        prop_assert!(history[sent.clone()].iter().all(|m| m.timestamp == t));
        prop_assert_eq!(sent.len(), history.iter().filter(|m| m.timestamp == t).count());
        prop_assert_eq!(
            unsent_at(&history, t),
            history.iter().rposition(|m| is_unsent_at(m, t))
        );
    }

    /// The same for a page sorted and older than the history, as stored pages come.
    #[test]
    fn older_pages_go_first(len in 0usize..30, before in 0usize..30) {
        let page: Vec<ChatMessage> = (0..len).map(|t| page_message((t as u64, 0), "new".into())).collect();
        let history: Vec<ChatMessage> = (0..before).map(|t| page_message((100 + t as u64, 1), "old".into())).collect();
        let mut at_once = history.clone();
        prop_assert_eq!(upsert_all(&mut at_once, page.clone()), (len, 0));
        prop_assert_eq!(at_once, page.into_iter().chain(history).collect::<Vec<_>>());
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 20000, failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn snippet_highlights_a_match(body in text(), query in text()) {
        let (text, found) = snippet(&body, &query);
        if let Some(found) = found.clone() {
            let part = text.get(found.clone());
            prop_assert!(part.is_some_and(|p| !p.is_empty()), "{found:?} in {text:?}");
            let shown: Vec<String> = part.unwrap().chars().map(fold).collect();
            let wanted: Vec<String> = query.trim().to_lowercase().chars().map(fold).collect();
            prop_assert_eq!(shown, wanted, "{:?} {:?} -> {:?} {:?}", body, query, text, found);
        }
    }

    /// When the flattened body contains the query (char by char, ignoring case), it is found.
    #[test]
    fn snippet_finds_what_is_there(prefix in text(), needle in "[a-zA-Z]{1,5}", suffix in text()) {
        let body = format!("{prefix}{needle}{suffix}");
        let (_, found) = snippet(&body, &needle);
        let flat: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
        let same_len = flat.to_lowercase().chars().count() == flat.chars().count();
        prop_assert!(found.is_some() || !same_len, "{body:?} / {needle:?}");
    }

    #[test]
    fn initials_are_at_most_two(title in text()) {
        let initials = initials(&title);
        prop_assert!(!initials.is_empty());
        prop_assert!(initials.chars().count() <= 2, "{title:?} -> {initials:?}");
    }

    #[test]
    fn formatting_never_panics(t in any::<u64>(), now in any::<u64>(), len in any::<usize>(), id in ".*") {
        let _ = format_size(t);
        let _ = format_remaining(t, now);
        let _ = format_list_time(t);
        let _ = format_time(t);
        let _ = format_clock(t);
        let _ = looks_like_number(&id);
        let index = avatar_index(&ConversationId(id), len);
        prop_assert!(index < len.max(1));
    }

    #[test]
    fn list_time_and_day_labels_never_panic(t in any::<u64>(), n in any::<i64>()) {
        let now = jiff::Timestamp::from_millisecond(n.rem_euclid(253_402_207_200_000 - 86_400_000))
            .unwrap()
            .to_zoned(jiff::tz::TimeZone::system());
        let _ = format_list_time_at(t, &now);
        let _ = day_label(t, &now);
    }

    #[test]
    fn timers_read_back(seconds in prop_oneof![1u32..100_000, any::<u32>()]) {
        let text = crate::format_timer(seconds);
        let (n, unit) = text.split_once(' ').unwrap();
        let n: u64 = n.parse().unwrap();
        let size = match unit.trim_end_matches('s') {
            "week" => 604_800u64,
            "day" => 86_400,
            "hour" => 3_600,
            "minute" => 60,
            "second" => 1,
            other => panic!("{other}"),
        };
        prop_assert_eq!(n * size, u64::from(seconds), "{}", text);
        prop_assert_eq!(n == 1, !unit.ends_with('s'), "{}", text);
        let _ = crate::timer_notice("You", seconds);
    }

    #[test]
    fn mentions_are_where_they_say(body in text(), names in prop::collection::vec("[a-zA-Z é🦀]{0,5}", 0..4)) {
        let people: Vec<Person> = names
            .iter()
            .enumerate()
            .map(|(i, n)| Person { service_id: format!("id{i}"), name: n.clone() })
            .collect();
        let mentions = mentions_in(&body, &people);
        let mut last_end = 0;
        for m in &mentions {
            prop_assert!(m.start < m.end && m.end <= body.len());
            prop_assert!(body.is_char_boundary(m.start) && body.is_char_boundary(m.end));
            prop_assert!(m.start >= last_end, "overlap {mentions:?}");
            last_end = m.end;
            let person = people.iter().find(|p| p.service_id == m.service_id).unwrap();
            prop_assert_eq!(&body[m.start..m.end], format!("@{}", person.name));
        }
        if let Some(query) = mention_query(&body) {
            prop_assert!(body.ends_with(query));
            prop_assert!(body[..body.len() - query.len()].ends_with('@'));
        }
    }
}
