# Updates

An installed Windows b33p (`%LOCALAPPDATA%\b33p\b33p.exe`, see `b33p-slint/src/install.rs`) can
update itself from GitHub's latest release, if the user turns on Settings → Updates → **Check for
updates** (off by default: it is traffic b33p makes for nothing else). It asks
`https://api.github.com/repos/barafael/beep/releases/latest` once the chats are there, then once a
day, and offers a newer version above the chat list and in Settings. **Update and restart**
streams the zip to disk, checks it against its minisign signature, takes `b33p.exe` out of it,
renames the running exe aside, moves the new one in and starts it.

A signature counts only if its trusted comment is exactly `b33p <version> <file>`, for example
`b33p 0.3.0 b33p-0.3.0-windows-x86_64.zip`. So an older signed build can't be served as a newer
one, nor one platform's zip as another's.

The AppImage could follow the same way; it doesn't yet. Distribution packages update themselves.

## The key: one-time setup by the repository's owner

Until this is done, releases are unsigned, builds carry no key, and the setting doesn't show.

1. Make a key pair without a password (the workflow can't type one), on a machine you trust:

   ```sh
   minisign -G -W -p b33p-update.pub -s b33p-update.key
   ```

2. The public key is the second line of `b33p-update.pub` (starting with `RW`). Set it as a
   repository **variable** (Settings → Secrets and variables → Actions → Variables):
   `B33P_UPDATE_PUBLIC_KEY`. The release builds read it at compile time (`option_env!` in
   `b33p-core/src/update.rs`).

3. Set the whole of `b33p-update.key` (both lines) as a repository **secret**:
   `B33P_MINISIGN_KEY`. The release job signs every archive with it, checks each signature
   against the variable, and uploads the `.minisig` files with the release. Without the secret it
   skips signing with a notice.

4. Keep an offline copy of `b33p-update.key`, then delete it from that machine. Without it,
   installed copies can't verify updates any more, and everyone has to download the next
   version by hand.

Rotating the key: release a version built with the new public key, signed with the old secret;
then replace the secret.

A release made before step 3 has no signatures, and installed copies don't offer it. Only a
version greater than the running one is offered, so a bad release is fixed by releasing a higher
version, never by moving a tag.
