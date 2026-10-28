# A nightly tour run

`nightly.sh` runs the tour (`cargo testbox run tour`, 6 rounds unless told otherwise) on what
`origin/main` is now, in a worktree of its own (`../beep-nightly` beside the repository), with
a box of its own (`--instance nightly`), which it takes down at the end. So it neither touches a
worktree in use nor waits for another box. Each run's output is in that worktree's `target/testbox-nightly-runs/<when>.log`,
and its records are under `target/testbox-nightly-record/run/<when it started>/`.

Run it once by hand with `testbox/nightly/nightly.sh 2`.

To run it every night at about 03:00 as a systemd user service:

```sh
sed "s|@REPO@|$PWD|" testbox/nightly/b33p-testbox-nightly.service \
    > ~/.config/systemd/user/b33p-testbox-nightly.service
cp testbox/nightly/b33p-testbox-nightly.timer ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now b33p-testbox-nightly.timer
```

A run's summary goes to the journal: `journalctl --user -u b33p-testbox-nightly`. The service
runs at a lower CPU and IO priority, and missed nights run at the next start
(`Persistent=true`). It needs what any box needs (rootless Podman, and `/dev/kvm` only for
Android, which the tour doesn't use). `systemctl --user disable --now b33p-testbox-nightly.timer`
stops it.
