# b33p's documents

What there is to read beyond the [README](../README.md), and which of it is kept current.

## Kept current

- [PLAN.md](../PLAN.md): where b33p stands and what comes next.
- [TESTPLAN.md](../TESTPLAN.md): what is checked by hand, and which checks the testbox does.
- [CHANGELOG.md](../CHANGELOG.md): what changed, release by release.
- [updates.md](updates.md): how releases are signed and how an installed b33p updates itself.
- [testbox/README.md](../testbox/README.md): the test box, what it needs and how to run it;
  [container-logs.md](../testbox/container-logs.md) (its log driver) and
  [nightly/README.md](../testbox/nightly/README.md) (the nightly tour) beside it.
- Bringing other clients into the test box:
  [signal-cli](onboarding-signal-cli.md), [signalmeow](onboarding-signalmeow.md),
  [Molly](onboarding-molly.md), [the presage family](onboarding-presage-family.md) (gurk-rs and
  Flare's backend).
- [.devcontainer/README.md](../.devcontainer/README.md): the development container.

## Plans and open work

Every open item is in [PLAN.md](../PLAN.md); it says which branch of each fork carries what
(section 2). The designs and findings behind its items:

- [actors.md](actors.md): moving the signal backend to actors, step by step.
- [account-parity.md](account-parity.md): what a long-lived account has that the test box's
  don't.
- [sigwin.md](sigwin.md): what is left to take from sigwin, a client on the same stack.
- [testbox-review.md](testbox-review.md): the test box looked back on, October 2026.
- Findings logs, kept as the runs go; code comments cite their finding numbers:
  [testbox-tour.md](testbox-tour.md), [testbox-stress.md](testbox-stress.md),
  [testbox-wire.md](testbox-wire.md).

## Records (dated, kept as written)

Test box runs and investigations, oldest first:

- [testbox-runs-2026-10-31.md](testbox-runs-2026-10-31.md): main through 2026-11-07, run after
  run, and the coverage it led to.
- [review-plan.md](review-plan.md): the UI review of 2026-10-24 and its fixes.
- [testbox-correctness-runs.md](testbox-correctness-runs.md) and
  [testbox-extended-runs.md](testbox-extended-runs.md): 2026-11-08.
- [performance-slint-frames.md](performance-slint-frames.md): why the Slint window redrew 60
  frames a second while messages arrived (2026-11-09).
- [testbox-sceptical-run-2026-11-11.md](testbox-sceptical-run-2026-11-11.md), with its
  [mutations](testbox-sceptical-run-2026-11-11-mutations/README.md) and its findings followed up
  in [silent loss](testbox-silent-loss-2026-11-11.md),
  [soak memory](testbox-soak-memory-2026-11-11.md),
  [speed and isolation](testbox-speed-isolation-2026-11-11.md) and
  [the storage manifest](storage-manifest-2026-11-11.md).
- [testbox-clients-run-2026-11-13.md](testbox-clients-run-2026-11-13.md): every Signal client
  in one box.
- [testbox-media-traffic-2026-11-19.md](testbox-media-traffic-2026-11-19.md): media on the wire,
  b33p against Signal's apps.
- [web-demo.md](web-demo.md): a study of running the mock in a browser.
