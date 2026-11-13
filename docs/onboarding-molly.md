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

## What was done

(Filled in as the work goes on.)
