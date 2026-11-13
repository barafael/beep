# Onboarding Molly into the testbox, 2026-11-13

Molly (mollyim/mollyim-android, `892d626`, 2026-09-08) is a hardened fork of Signal-Android. This
is the feasibility study written before building anything, then what was built and found.

## What Molly is, as a test subject

Not an independent implementation. It is Signal-Android's code (same `org.thoughtcrime.securesms`
classes, same `lib/libsignal-service`, same libsignal-net chat) at an older upstream release,
with Molly's additions (a passphrase lock for the database, a proxy-only mode, UnifiedPush,
MollySocket). Its test value is therefore **the same Java/Kotlin stack at 8.19.2 instead of the
box's 8.32.1**: a client 13 minor releases (about three months) older than the server it talks
to, as real users are. Nothing about the wire is new except what that age and Molly's changes make
different.

| | box's Signal-Android | Molly |
|---|---|---|
| app | 8.32.1 (`e9b569e`, 2026-10-16) | 8.19.2 (versionCode 1719) |
| libsignal | 0.105.0 | `im.molly:libsignal-client 0.96.3-1` |
| applicationId | `org.thoughtcrime.securesms` (+ `.local` flavor) | `im.molly.app` (`basePackageId`) |
| server pin | Signal-Server `ccc22f9` (2026-10-15) | the same box |

## (1) libsignal: the local override

Molly's libsignal is upstream v0.96.3 plus 37 commits of build plumbing (compared through the
GitHub API: Dockerfile, publishing under group `im.molly`, version `0.96.3-1`, a proxy-only mode),
no protocol changes.

- libsignal-net chat is used by the app (`LibSignalChatConnection.kt`,
  `ApplicationDependencyProvider.provideLibsignalNetwork`), as in the box's Android.
- `TESTING_ConnectionManager_newLocalOverride` exists at 0.96.3 with the same signature and the
  same `#[bridge_fn(jni = false, ffi = false)]`, and `net_env` is node-only there too: the very
  situation `testbox/patches/libsignal-local.patch` fixes at 0.105.0.
- **Measured**: `patch --dry-run` of that patch on Molly's libsignal tag `v0.96.3-1`: every hunk
  applies (the Kotlin one with fuzz 2, offsets of 4 to 93 lines) except the version bump in
  `java/build.gradle`, which Molly's version string (`0.96.3-1`, group `im.molly`) makes moot.
- Molly can build libsignal from source: `settings.gradle.kts` includes it as a composite build
  (`libsignalClientPath` in gradle.properties) and substitutes `im.molly:libsignal-client` and
  `libsignal-android` with it. No publishing to `~/.m2`, no verification-metadata checksums for it.

Verdict: a rebase of the patch, not new work.

## (2) The app: the `local` flavor

`patch --dry-run` of `testbox/patches/signal-android-local.patch` on Molly's tree: the
`ApplicationDependencyProvider` hunks and the `buildConfigField` hunks apply (fuzz 1 to 2); two
hunks fail and need a hand rebase: the `selectableVariants` list in `app/build.gradle.kts` (Molly's
build has a different variant machinery) and the libsignal version line in
`gradle/libs.versions.toml` (Molly's is `im.molly`, and becomes irrelevant with the composite
build). Molly keeps the `provideLibsignalNetwork` seam, so the server values reach the app the
same way (BuildConfig fields from the JSON `signalLocalConfig` names).

## (3) The driver

`testbox/tools/src/bin/android.rs` drives the app through an instrumentation agent by labels
(`tap("Continue")`, "Phone number", "Verification code", "Disable PIN", "First name (required)",
"Chats", then Settings, "Internal Preferences", "Enable ANR-induced crashing"). What differs for
Molly, known before running it:

- the package: `PACKAGE_PREFIX = "org.thoughtcrime.securesms"` and the `.local` suffix filter; Molly's
  id is `im.molly.app` (+ the flavor's suffix).
- Molly's first run has its own screens (`PassphraseCreateActivity`, `PassphrasePromptActivity`,
  `ScreenLockController`): the database passphrase set-up the driver doesn't know.
- an 8.19.2 UI: labels moved between releases; "Internal Preferences" exists only in debug
  builds, and Molly's `local` build would have to be one.

Not knowable without a run: how many of these labels differ. The driver's UI flow is the long pole.

## (4) Protocol compatibility with the box's server

The box's Signal-Server is 13 minor releases newer than the app. Server-side removal of an endpoint
Molly 8.19.2 still calls would break registration or sending. Not determinable from the sources;
the first registration is the test. In its favour: the same libsignal-net chat, the same REST
verification session (`/v1/verification/session`, which the box has had since Android's first run)
and a build timestamp (the last commit's, 2026-09-08) that has not expired (Signal's apps refuse
to send 90 days after the build).

## (5) Cost of a second emulator

Android's container is 3.4 GB of image and an emulator with KVM (`-memory` in
`testbox/tools/src/bin/android.rs`); a pod has one network namespace, so a second emulator would
fight the first for adb's server port (5037) and the emulator's console ports; the driver, `Service`
and `Client` assume one. **So the minimal path is a box where the Android container is Molly's**
(`--molly` in place of `--android`), which reuses everything and costs a box, not a second emulator.
Molly then runs the phases' own checks against b33p, one box at a time.

## Decision

Feasible in a bounded step: rebuild the libsignal patch on `v0.96.3-1`, rebase the app patch on
Molly's tree, build the APK in an image beside Android's (the `signal` stage swapped for Molly,
libsignal as a composite build), generalise the driver's package constants, and try a
registration on an `--instance molly` box. The registration flow's labels are discovered by
running it; the doc below records what was found.

## Result (2026-11-13): it works; 15 of 18 `android` checks pass, the 3 failures are driver label differences

`cargo testbox --instance molly test android --android-app molly --screenshots`, instance `molly`,
sixth run (the first five each stopped at the next screen of Molly's that differs, below). The
setup, 16 checks: **all passed**, including the ones that need the newer server and b33p's own
code: Molly registers on the box's Signal-Server (`ccc22f9`, 13 minor releases past its app) with
the box's captcha and any code, writes a note to itself, **b33p links to it with the message
history** (Molly's `Transfer message history`), the peer's first message reaches b33p and Molly.
Then the `android` phase: text each way with the peer and Desktop, delivery receipts, a reply
that quotes, an edit, a note b33p writes to itself showing on Molly, a long text each way: passed.
No client lost a message, none crashed, b33p logged no unexpected ERROR.

The 3 that failed, each a thing the driver looks for on 8.32's screen and not on 8.19's, not
anything on the wire:

- **a reaction**: the reaction bar is on screen after the long press (screenshot
  `android/08-a-reaction-…-android.png`, the six emoji and the long-press menu), but the driver's
  check for "the bar with its heart" doesn't find it: Molly's heart has another accessibility
  label. Cause found by the screenshot, label not yet read.
- **a delete for everyone**: a one-time dialog ("Deleting is now synced across all of your
  devices", OK) is over the menu; the driver waits for "Delete for everyone" and never taps OK.
  Cause found in the failure's own screen text.
- **a timer**: the next step after the delete, with that dialog presumably still open (its
  failure text starts with the same status bar and was truncated before the dialog's text); not
  separately confirmed, so it is unexplained until the dialog is dismissed and the check rerun.

The records of the run are in the scratchpad, `molly-records/run6` (every container's log,
logcat, screenshots for each check).

### Findings about Molly and the box

- Molly's `SequentialDns` refuses loopback answers for Signal's service hosts (a hardening
  Signal-Android doesn't have); the box's server is on the device's `localhost`, so the local flavor
  is an exception (`IS_LOCAL`). Without it nothing would connect, and it says that Molly would
  never talk to a server on a loopback or private address as a deliberate protection.
- The box's Signal-Server, 13 minor releases newer than the app, still accepts its registration,
  its websocket (libsignal-net 0.96.3) and its linked-device provisioning with history transfer:
  nothing in the box's server needed to change for an app about three months old.
- 8.19's first run asks about a database passphrase; its phone-number screen has a separate
  country-code field (empty without a SIM); its code entry is an on-screen number pad ("Verification
  Code", capital C); its PIN screen comes after the profile, with "More options" for a menu where
  8.32 has "Open menu"; its Link-a-device screen has a "Scan QR code" button first. The driver
  now handles all of these (testbox/tools/src/bin/android.rs `register`, `disable_pin`,
  `link_device`).
- The APK is universal (`app-local-website-debug.apk`), not per ABI.
- Cost: libsignal built in 8 m 45 s, the app in about 10 m on a loaded 32-core machine; the image
  adds the SDK platform android-36.1 on top of the box's Android image. One emulator at a time:
  `--android-app molly` replaces Signal-Android's container, it does not add a second emulator.

### To finish

Dismiss the delete dialog (tap "OK" when shown) and read the heart's label in the driver, rerun
the phase, then the `groups`, `storage` and `large-group` phases (`test android groups storage
large-group --android-app molly`) which were not run.

## What was done

- `testbox/patches/libsignal-molly-local.patch`: Signal-Android's libsignal patch (the Java
  `Network.localTestServer`, the JNI bridge for `TESTING_ConnectionManager_newLocalOverride`, the
  testing crate's `net_env`) on Molly's fork at its tag; no version bump, the coordinates stay
  `im.molly:libsignal-*:0.96.3-1`.
- `testbox/patches/molly-local.patch`: a `local` flavor in the `environment` dimension (variant
  `localWebsiteDebug`, applicationId `im.molly.app.local`), the server values from the same
  `signalLocalConfig` JSON, `Network.localTestServer` in `provideLibsignalNetwork`, and
  **an exception to Molly's loopback filter**: `SequentialDns` drops loopback and 0.0.0.0 answers
  for Signal's service hosts ("MOLLY: Filter out invalid addresses", except `Environment.IS_DEV`),
  which is a hardening Signal-Android does not have and which would make the box's
  `localhost:<port>` unreachable; `Environment.IS_LOCAL` skips it.
- `testbox/molly/Containerfile`: Android's image with Molly's stages: libsignal from Molly's fork
  at the commit its annotated tag `v0.96.3-1` points at (`08f98369`), on the nightly its
  `rust-toolchain` names; Molly at `892d626`; Molly's `verification-metadata.xml` with the
  checksums of the two libsignal components replaced by those of the build (`verification.py`,
  tested against Molly's real metadata: four checksums replaced, nothing else touched); Android
  platform 36.1 (Molly's `compileSdk`); Signal-Android's pin only for the Gradle wrapper and plugin
  the driver's agent builds with.
- `xtask`: `Client::Molly` (its container is `android`: `testbox/molly/pod.yaml`, the driver, its
  socket and display unchanged), `--android-app signal|molly` / `TESTBOX_ANDROID_APP` on `up`,
  `test` and `run`, so that the `android`, `groups`, `storage` and `large-group` phases run on
  Molly as they do on Signal-Android.
- The driver (`testbox/tools/src/bin/android.rs`): the package prefix from
  `ANDROID_PACKAGE_PREFIX` (`im.molly.app`), and a first step in `register` that taps Molly's
  passphrase question ("No thanks", `PassphraseCreateActivity`) if it is the first screen.
