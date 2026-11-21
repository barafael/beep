# Fork branches

b33p builds on forks of presage, libsignal-service-rs, rusqlite and Slint. What each carries:

- **presage, libsignal-service-rs, rusqlite**: branch `langis/integration` of each fork, upstream
  main plus our open pull requests and a few changes without one yet. The comments above each
  pin in the workspace `Cargo.toml` list them, theme by theme with their PR numbers, and are
  updated at each rebase; [PLAN.md §2](../PLAN.md) says how to rebuild them. They are the only
  list: this file no longer repeats them.
- **Slint**: below, the one place its topics are tracked.

The `testbox-green` branch's commits and the testbox runs of October (#69–#84) were listed here
until 2026-11-21; `git log -- pending/branches.md` has them.

## Slint (barafael/slint)

b33p pins `slint/integration-branch` (`27f6992`, 2026-11-21): upstream master `2dbdba1` with the
branches below merged in order by the branch's own `integration.sh` (`./integration.sh push`
tags the old tip `integration/<date>-<commit>`; the previous pin is `integration/2026-11-21-076293d90`).
A topic that conflicts with an earlier one is listed as `merge/<topic>`: the topic with the earlier
one merged in by hand.

| Topic | Upstream | State |
|---|---|---|
| `no-op-animation` | slint-ui/slint#14823 | ogoffart's review answered 11-09; waiting on him |
| `scrollbar-thumb-animation` | #14841 | ready since 11-20, no review yet |
| `text-cursor-blink-timeout` | #14855 | ready since 11-20, spell check fixed, no review yet |
| `inline-code-padding` | #14721 | parley 0.13 needs a U+FFFC under each in-flow inline box: the left padding's box now has one in the layout's text (`34293320d`); asked on the PR whether that's sane |
| `merge/edualc-text` | none (felintusun's branches) | `edualc-text` with `inline-code-padding` merged in by hand |
| `merge/incremental-text-shaping` | #14953 | overlaps ogoffart's #14742 (plain text only; ours covers StyledText and content widths): rebase onto it as a follow-up. Has `merge/edualc-text` merged in by hand; content widths shape the same U+FFFC text as drawing |
| `fix/popup-follows-parent` | #15091 | draft, opened 11-21: an open popup moves when a layout moves its parent (the chat header's menu on a resize) |
