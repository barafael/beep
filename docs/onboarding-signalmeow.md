# Onboarding signalmeow into the testbox, 2026-11-13

signalmeow is the Go implementation of Signal's client protocol inside
[mautrix-signal](https://github.com/mautrix/signal) (`pkg/signalmeow`, at 01626c1, 2026-11-12). It is
the one Signal client in the box that shares **no protocol code** with b33p's stack: not presage,
not libsignal-service (Rust, or Java as signal-cli and Signal-Android have it). Its websocket, REST,
gRPC, protobuf, group and zkgroup-credential code is its own; it takes libsignal only for the
cryptography (`pkg/libsignalgo`, cgo over libsignal's FFI, v0.106.1). So what it agrees with
here, b33p and Signal-Server agree on independently.

`cargo testbox --instance <name> test signalmeow` (add `--capture` for the wire) runs one box of
its own; `cargo testbox up --signalmeow` plays the container beside the others.

## Result

27 checks, all passing (the phase took 24 s once the images were built). The runs before it each
failed on a check of mine, root-caused below, none on signalmeow or b33p:

| Check | What it shows |
|---|---|
| signalmeow links as a secondary device to a presage-cli primary | `/v1/websocket/provisioning/`, `PUT /v1/devices/link` with the box's keys |
| text signalmeow → presage-cli peer, and back | pairwise prekey sessions, sealed sender both ways |
| signalmeow sends a delivery receipt | the peer's `sync` prints `got Delivery receipt` |
| text signalmeow → b33p (a message request, accepted), and b33p → signalmeow; b33p shows the delivery | b33p's session with a client that is not libsignal-service |
| a 300 KiB file signalmeow → b33p (saved), peer → signalmeow, b33p → signalmeow, each by SHA-256 | tus on CDN3, encryption, digest and MAC across implementations |
| a group b33p makes of the peer and signalmeow: b33p's text reaches signalmeow, signalmeow's reaches b33p and the peer | the storage service's group fetch, zkgroup auth credentials, sender keys (`multi_recipient`) |
| no client lost a message, none crashed, b33p logged no unexpected ERROR | the part-ending checks |

## What it takes to point signalmeow at the box

Everything it needs is compiled in; one patch (`testbox/patches/signalmeow-local.patch`, 150 lines,
most of it one new file) makes it configurable:

- `pkg/signalmeow/web/web.go`: the hostnames (`APIHostname`, `StorageHostname`, `CDN1Hostname`…)
  become variables (they are Go constants, so the host string that goes into `"https://" + host`
  can also carry the port); `grpc.go`: `GRPCTarget` likewise.
- `pkg/signalmeow/web/box.go` (new): when `SIGNALMEOW_BOX_CONFIG` names the box's
  `/keys/desktop-config.json`, an `init()` takes the hosts and ports from it, makes the box's CA
  the TLS root (`SignalCertPool` and `SignalTLSConfig.RootCAs`), and overrides
  `libsignalgo.ProdTrustRoots` (the unidentified-sender trust roots) and, through
  `web.BoxServerPublicParams`, the zkgroup server public parameters (`misc.go`).
  Without the variable nothing changes.
- **The chat host differs from Desktop's.** `serverUrl` in the config is the omnibus port
  (8445), which is HTTP/2 only: signalmeow's websocket (`coder/websocket`) is an HTTP/1.1
  upgrade, and its first run died with `malformed HTTP status code "\x00\x00\b\x00…"` (an HTTP/2
  SETTINGS frame). Jetty's HTTP/1.1 port (8444, the one libsignal-service uses) serves
  `/v1/websocket/` and `/v1/websocket/provisioning/`, so `SIGNALMEOW_BOX_CHAT_HOST=localhost:8444`
  (testbox/signalmeow/pod.yaml) overrides the websocket's host; gRPC stays on 8445.
- **libsignal needs no patch.** signalmeow's chat is not libsignal-net, so it does not need the
  `TESTING_ConnectionManager_newLocalOverride` bridge that Signal-Android's image has to
  publish (testbox/patches/libsignal-local.patch). The image builds libsignal's FFI
  (`libsignal_ffi.a`, tag v0.106.1, as mautrix-signal's submodule pins it) unpatched.

The image (testbox/signalmeow/Containerfile, 177 MB) builds libsignal's FFI from source, then the
driver against the patched mautrix-signal source (`go.mod`'s `replace`, `-mod=readonly` with a
`go.sum`), with `gofmt` and `go vet` as gates; Rust 1.98 toolchain from the pinned `rust:1-trixie`,
`golang:1.27-trixie` by digest. Network only while it builds.

## The driver (`testbox/signalmeow/driver`)

A command line, as presage-cli is, so xtask drives it with `podman exec` the same way
(`xtask/src/checks/signalmeow.rs`, whose methods mirror `Presage`'s): `link`, `whoami`, `send`,
`send-file`, `send-group` (the group's master key as hex, as b33p and presage-cli name it, or
base64), `read`, `sync`, `list-messages`. It connects, prints what arrives meanwhile in
presage-cli's format (`From <aci> @ <timestamp>: <text>`; for an attachment its size and SHA-256,
after downloading and decrypting it), and exits. `serve` stays connected and speaks JSON lines
(`{"id":1,"op":"send","to":"<aci>","text":"…"}` answered by `{"event":"answer","id":1,"ok":true,
"timestamp":…}`, events as `{"event":"message",…}`), for the load phases to come.

signalmeow can **link, not register**: it has no registration code (the bridge only ever links),
so its account is a presage-cli primary's (`Account::Meow` in xtask).

## What it does differently on the wire (from `--capture`, compare.md)

- **Two websockets per start**: an authenticated and an unauthenticated one (`GET /v1/websocket/`
  twice), where libsignal-service has one. It sends `X-Signal-Agent: MAU` and `Accept-Encoding:
  gzip`; its agent is `signalmeow/0.1.0 libsignal/v0.106.1 go/1.27.4` (xtask/src/wire/mod.rs
  now names it, as it did not know the agent).
- **`PUT /v1/devices/capabilities` on every connect** (nine times in the run); b33p and presage-cli
  put them in `PUT /v1/accounts/attributes`.
- **Pre-keys.** The key bundles signalmeow fetched (`GET /v2/keys/<aci>/*?pq=true`, with `pq`,
  which libsignal-service's fetch doesn't ask for) for the presage-cli accounts had
  `preKey: null`: no one-time EC key, so its first messages went on the signed and Kyber keys
  alone, and decrypted. Its own one-time keys go up from its `keyCheckLoop` on the first connect
  (`GET /v2/keys?identity=aci`, then `PUT /v2/keys?identity=aci` and `…=pni`, 227 KB each),
  which is what the wire shows right after its first send.
- **Group send**: `GET /v1/certificate/auth/group?…&v101=true&zkcCredential=true` (the ZKC
  credential form: b33p and presage-cli use `pniAsServiceId=`), `GET /v2/groups` on the storage
  service (HTTP/1.1, `Authorization` the group credential), and one
  `PUT /v1/messages/multi_recipient?ts=…&urgent=…&online=…` (b33p sends the same with `story=…`).
  A sender key distribution message goes pairwise first; b33p logs it as
  `libsignal_service::cipher: empty decrypted content` (a WARN in presage, for a message whose
  content is only the key).
- **tus uploads in one request**: signalmeow `POST`s the body with the creation
  (`Content-Type: application/offset+octet-stream`, the creation-with-upload extension): one
  round trip, `201`. b33p and presage-cli `POST` empty, `HEAD`, then `PATCH` (three). CDN3 accepts
  both. See below.
- **No gRPC at all.** Nothing in the run reached the omnibus port (8445): signalmeow's gRPC
  client is used for username lookups and spam reports only. The box's gRPC side stays covered
  by Desktop and Android alone.
- Sealed sender: most of the envelopes the server delivered to signalmeow were `type=6`
  (`UNIDENTIFIED_SENDER`) without a source (the sealed sender b33p and the peer use), and it
  decrypted them against the box's trust root.
- It never asked for profiles, `whoami`, `POST /v2/keys/check` or the storage manifest, which
  b33p and presage-cli do: it takes its profile key from the provisioning message and keeps no
  storage state of its own here.

No request of signalmeow's was answered with a 4xx or 5xx in the passing runs, and none of the
79 connections of the last run failed to decode.

## Failures on the way, and their causes

None of these was a defect of signalmeow, b33p or the box; each was found and fixed the day it
showed.

1. **`link` failed: HTTP/2 SETTINGS frame where an HTTP/1.1 status was expected.** The config's
   `serverUrl` is the omnibus port, h2 only (above).
2. **"the peer's delivery receipt reaches signalmeow" failed.** presage-cli sends no receipts
   (xtask/src/checks/core.rs says so, presage-cli/src/main.rs only prints one). The check now asks
   the other way: signalmeow sent one for the peer's text (`got Delivery receipt` in the peer's
   sync).
3. **"a file from signalmeow reaches b33p, downloaded" timed out** (twice, the second after I
   added `FetchAttachment`). b33p fetches an attachment on its own only if it shows it inline
   (`state/attachments.rs fetch_attachment`'s `shown_inline`); a file is downloaded when it is
   saved, and the attachments phase uses `SaveAttachment` for files. The check does too now.
4. Build-time only: the FFI needs `libprotobuf-dev` (protoc's `google/protobuf/duration.proto`)
   and the Go link needs `zlib1g-dev` (libsignalgo's `-lz`).

## Observations for b33p

- **tus creation-with-upload.** signalmeow's one-request upload is accepted by CDN3 and is
  cheaper than b33p's POST, HEAD, PATCH. For attachments under the size where resuming
  matters, b33p could do the same (the box's wire record has both, side by side: compare.md's
  "The same request, made differently"). Not changed here: it is presage's upload code
  (libsignal-service `upload_attachment`), and Signal's own clients resume.
- **WARN noise from presage on a legitimate message.** `libsignal_service::cipher: empty decrypted
  content` is a sender key distribution message arriving alone, as every other client sends it; it
  is worth a debug, not a warning, but it is presage's line.
- **Bad MAC then "decrypted with PREVIOUS session state"** on b33p's log is the ordinary
  out-of-order case (signalmeow's receipt and b33p's reply crossed): libsignal's, correct, but it
  prints `WARN … Bad Mac!` for it, which a reader of b33p's log will take for a fault.
- **The check "b33p logged no unexpected error" held**: with a client of another implementation
  writing to it, b33p logged only the known kinds (`could not create sync message from a group
  message`, and the connection the box's end drops when the server is stopped for its log).
- Nothing in b33p could be removed or simplified on the strength of this run alone; the run is a
  correctness check, the performance and load phases are the next step (below).

## What this does not show yet

- **Load, races and soak.** The phase is a smoke test: one conversation each way, a 300 KiB
  file, one group. The driver has `serve` for what comes next: several signalmeow accounts
  against the volume, interrupt and concurrency phases, a soak with the box's memory watch. None
  of it ran.
- Edits, reactions, deletes, typing, stories, disappearing timers, read receipts as checks (the
  driver has `read`; no check uses it), voice notes, stickers, profile pictures, storage service
  sync of settings (signalmeow reads `GetStorage*` only on request), and a signalmeow device
  linked to b33p's primary as a *second* linked device (the fan-out of one account to a
  presage and a Go device).
- CDSI and SVR (not in the box), key transparency, and signalmeow's gRPC paths.
- signalmeow as the **peer/primary** of b33p: it cannot register, so b33p has only been linked
  to presage-cli and Android as primaries.
- The sender-key toggle: signalmeow's `enableSenderKeySend` (`sending.go`) has a pairwise
  fallback (`sendToGroup`) that this box never exercises.

## Files

- `testbox/signalmeow/{Containerfile,pod.yaml,driver/}`, `testbox/patches/signalmeow-local.patch`
- `xtask/src/checks/signalmeow.rs` (the phase and `Signalmeow`), `phase.rs` (`InteropPhase`),
  `pod.rs` (`Client::Signalmeow`, `Service::Signalmeow`, `spawn_exec`), `main.rs` (`up --signalmeow`),
  `wire/mod.rs` (the client's name), `checks/presage.rs` (`Account::Meow`), `testbox/pod.yaml` (its volume)
