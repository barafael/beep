# signal-cli in the testbox

[signal-cli](https://github.com/AsamK/signal-cli) v0.14.10 (8890c1b, 2026-11) is in the testbox as a
client of its own: `cargo testbox test signal-cli` (only when named), `cargo testbox up --signal-cli`
for a box with its container. It is the first client in the box that is neither presage nor
libsignal-service-rs, and not one of Signal's own.

## What it is, and why it counts as a third implementation

- Java 25. Its protocol layer is Turasa's fork of `libsignal-service-java`
  (`com.github.turasa:signal-network:2.15.3_unofficial_157`, from Signal-Android's sources), on
  `libsignal-client` 0.106.1 (its `libsignal-version` file) through JNI.
- The chat is libsignal-net's (`LibSignalChatConnection`): websockets and gRPC over HTTP/2 to the
  server's omnibus port. REST (registration, profiles, keys, groups, storage) is OkHttp to Jetty's
  HTTP/2 port. b33p and presage-cli use libsignal-service-rs' own websockets and HTTP/1.1
  (port 8444), so the two sides of every check between them share one HTTP stack and one
  encoding of the protocol; here they don't.
- What it shares with b33p is libsignal's Rust (zkgroup, the double ratchet, sealed sender,
  sender keys): a defect there still passes unseen, as with Desktop and Android.
- It says `Signal-Android/8.35.1 signal-cli/0.14.10` as its agent (`BaseConfig.USER_AGENT`, and
  libsignal-net's), so that Signal's servers take it for Android. The wire decoder (xtask/src/wire)
  now tells it from Android: `signal-cli` is a client of its own in `compare.md` and the
  transcripts (a test holds that).

## What the box needed

signal-cli has two environments, `LIVE` and `STAGING`, each a class of URLs, trust roots and public
parameters compiled in. Nothing takes a URL.

1. **libsignal-cli-local.patch**: `testbox/patches/libsignal-local.patch` (Android's) rebased on
   v0.106.1. Two hunks needed a hand: `TESTING_ConnectionManager_newLocalOverride` now takes
   `AsType<NonZeroU16, u16, { str_as_int(..) }>` parameters (the attribute line moved a few lines
   and the context with it), and `java/build.gradle`'s version line. The JNI testing library
   (`libsignal_jni_testing`) is built beside the production one by `build_jni.sh desktop`; the patch
   lets `net_env` compile for `jni` and exposes `Network.localTestServer`. Verified with
   `cargo check -p libsignal-jni-testing` on the host before the image build.
2. **signal-cli-local.patch**: when `SIGNAL_CLI_TESTBOX_CONFIG` names a file, `ServiceConfig` returns
   `TestboxConfig`'s environment and `SignalDependencies.getLibSignalNetwork` calls
   `Network.localTestServer`. `TestboxConfig` reads **`/keys/desktop-config.json`**, Desktop's
   config as `provision` writes it, at run time. Unlike Android's APK, the image doesn't depend on
   the box's keys: new keys need no rebuild. The URLs are Android's (Jetty HTTP/2 for REST,
   omnibus for the chat, `storageUrl`, the CDNs on Jetty), the CA becomes an in-memory BKS trust
   store (`BlacklistingTrustManager` opens `KeyStore.getInstance("BKS")`), the zkgroup, generic and
   backup public parameters and the sealed-sender trust root come from the same file.
   Contact discovery and SVR point at Jetty and are not used (the box runs neither).
3. **signal-cli's build excludes libsignal's testing library from the jar it ships**
   (`build.gradle.kts` `excludePatterns`, the `JarFileExcluder` transform). The patch empties the
   map. That is a hardening of theirs, not something the box defeats at run time.
4. The libsignal client jar is built in the image (`-PskipAndroid`: libsignal's Gradle configures
   its `:android` projects otherwise, and wants an SDK) and handed to signal-cli's own
   `-Plibsignal_client_path` switch, which exists for platforms without a published native library.

The image (`testbox/signal-cli/Containerfile`): Debian + Temurin 25 JRE copied from the pinned
image (as the server's image does with 26), the install tree of `installDist`, no network at run
time. The pod's container sleeps; checks `podman exec` signal-cli, one JVM per command (about 2 s
a command with its requests, by the log's timestamps), data in the `signal-cli` volume, one directory per account
(`/signal-cli/cli`, `/signal-cli/linked`), each with its `log.txt` (`--log-file`), which the
record keeps as `signal-cli-app.log`.

The libsignal jars are 200 and 250 MB: libsignal's release build keeps line tables
(`CARGO_PROFILE_RELEASE_DEBUG=1`). The image is 430 MB.

## The checks

`cargo testbox --instance <name> test signal-cli --capture` brings up its own box: b33p linked to a
presage-cli primary, a presage-cli peer, signal-cli registered as a primary of its own
(`+15555550110`; the server takes any code and the captcha `signalcaptcha://noop.noop.registration.noop`),
and signal-cli's and the peer's first messages accepted in b33p (contact discovery isn't in the
box). Then, in xtask/src/checks/signal_cli.rs:

| check | what it shows |
|---|---|
| text from b33p reaches signal-cli | sealed sender or not, b33p's session with a client that isn't libsignal-service-rs |
| b33p sends signal-cli a delivery receipt | the receipt names the time `send` printed |
| text goes both ways between signal-cli and a presage-cli account | |
| a picture from b33p reaches signal-cli, intact | CDN3 (tus) upload by b33p, signal-cli's download and decryption; compared by pixels: b33p sends a picture without its metadata, re-encoded, so its bytes aren't the file's |
| a picture from signal-cli reaches b33p, downloaded and decrypted | signal-cli's upload (the gRPC `GetUploadForm`, then tus) |
| ten texts from signal-cli in a row reach b33p, in order | |
| b33p makes a group of signal-cli and the peer; text b33p sends reaches signal-cli | signal-cli learns of a group from a message in it |
| text signal-cli sends to the group reaches b33p and the peer | signal-cli's sender-key send (`MessagesAnonymous/SendMultiRecipientMessage`) |
| text the peer sends to the group reaches signal-cli | |
| signal-cli's record of the group holds what was written in it | |
| signal-cli links to the primary b33p is linked to | provisioning by a client that isn't libsignal-service-rs |
| a text to the primary reaches b33p and linked signal-cli | fan-out to two linked devices, one of them signal-cli |
| what linked signal-cli sends reaches the peer and b33p | b33p lists it as sent, from the sync transcript |

Plus the end-of-part checks every phase has: no client lost a message (signal-cli's `exception`
lines count), none crashed, b33p logged no unexpected error.

## Results

On `main` at 806c673 plus this change, instance `cli`, `test signal-cli --capture`, 2026-11-13,
debug b33p (Slint):

| run | result | cause of each failure |
|---|---|---|
| 1 | setup 1 failed | the wrapper's, not the box's: `listAccounts` calls the ACI `aci`, the wrapper looked for `uuid`. Registration itself passed (patched libsignal, config and trust store worked first time) |
| 2 | phase 14 of 17 | three of the wrapper's: `receive` has `isDelivery`/`isRead` flags, not a `type`; `send -a` takes every argument after it, the recipient too (`No recipients given`); b33p sends a picture without its metadata, re-encoded (b33p-core `without_metadata`), so signal-cli's file is a different PNG of the same pixels (the check compares pixels now) |
| 3 | setup 14 passed, phase 17 passed, 98 s | |
| 4 | setup 14 passed, phase 17 passed, 97 s | check times within 0.3 s of run 3's, each |

No check was retried or loosened to pass (the three failures of run 2 were bugs of the checks, each
reproduced from the record before it was changed). Runs 3 and 4 had 147 connections each, every
one fully decoded.

## Observations, from the wire and the logs

From `wire/` of runs 2 to 4 (the records are not in the repository; `--capture` writes them) and
run 4's `signal-cli-app.log`.

- **signal-cli's linking has a defect of its own**: after `link` completes, `SyncStorageJob` fails
  with a `NullPointerException` at `StorageHelper.syncDataWithStorage` (`getSelfRecipientProfile()` is
  null: the linked account's own profile hasn't been fetched when the job runs). Seen in the one
  run that kept the log (run 4; the log file was added after run 3). The checks after it pass, so
  the account works; whether the storage sync is retried and succeeds later wasn't checked.
- **Each invocation starts from scratch on the wire**: it opens with a `PUT
  /v1/accounts/attributes`, a `GET /v1/accounts/whoami`, gRPC `Keys/GetPreKeyCount`s and a
  websocket (27, 27, 60 and 49 of them in run 4, for the phase's 20-odd commands), and a send adds
  `GET /v1/certificate/delivery`. b33p and presage-cli check their keys with `POST /v2/keys/check`
  instead; signal-cli never does. Not a defect, but what any load made of single commands
  measures.
- **Two requests only signal-cli makes**: the gRPC `Attachments/GetUploadForm` (b33p and
  presage-cli ask `GET /v4/attachments/form/upload`) and
  `MessagesAnonymous/SendMultiRecipientMessage` (its group send, one sender-key request). The
  server answers both; in this box nothing else exercises the first.
- **409 on `PUT /v1/messages/{aci}`, twice**, to the primary's account (the response of a send
  that used a stale device list; the likeliest cause is the primary gaining devices while signal-cli
  linked, not checked): signal-cli recovered each time, no message was lost.
- Its TLS hellos (fingerprints `114d1a35` JDK/OkHttp, `ceb40329` libsignal-net, `fa577457`) offer
  `h2` and `http/1.1`; b33p's and presage-cli's one hello (`e60eff0e`) offers `http/1.1` or nothing.
  The server's JDK TLS engine (`grpc.sslProvider: JDK`) takes all of them, and `--capture` decodes
  all of them: 0 connections not wholly decoded.
- Connections that carry no byte either way: 36 in run 3, 29 of them to the omnibus port, 7 to CDN3.
  The baseline linked box has some too (7 on 8445, 8 on 8444), so they aren't signal-cli's alone;
  in this box they arrive at the rhythm of its commands (about 2 s apart) but never within 50 ms of
  its own connections. Cause not found.
- **What b33p does with a picture** (not signal-cli's): the 47,940-byte `picture.png` reached signal-cli as
  a 61,026-byte PNG, the same pixels. b33p redraws a PNG or JPEG from its pixels to take the
  metadata out (`without_metadata`, "as Signal's apps send them"); a JPEG is re-encoded at quality 90,
  lossy. For a PNG this made the file 27% larger and cost a decode and an encode; a part of b33p
  that could be simpler (drop the metadata chunks and segments in place) with the same privacy
  and no loss. Not changed here.
- signal-cli says it is Signal-Android (`Signal-Android/8.35.1 signal-cli/0.14.10`, also in
  libsignal-net's agent). An unmodified wire decoder filed it under Android's column; it has its
  own now.

## What isn't done

- The Presage-shaped surface is `SignalCli` in xtask/src/checks/signal_cli.rs: `register`, `link`,
  `send`, `send_to_group`, `send_all`, `send_file(s)`, `messages_with`, `sync`, `sync_printing`.
  signal-cli stores no messages, so `messages_with` is the record of the checks' own receives.
- One JVM per command is slow (about 10 s for a send and a receive) and noisy on the wire (see
  below). For load, signal-cli's JSON-RPC daemon (`signal-cli jsonRpc`, or `daemon --socket`) keeps
  one connection and one process; the wrapper doesn't have it yet.
- A decryption failure in signal-cli appears as an `exception` in `receive --output=json`; the
  parser and the "no client lost a message" bookkeeping are there and unit-tested, but no check
  has made signal-cli fail to decrypt.
