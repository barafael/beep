# Media on the wire: b33p, Signal-Desktop and Signal-Android (2026-11-19)

Why chats with pictures are slow in b33p, from a `--capture` run of the new `media` phase on the Android box
(`TESTBOX_MEDIA_PHOTO=<a 4032×3024, 7.3 MB JPEG> cargo testbox test --capture media`), and the pinned
sources of Signal-Desktop (`f3040f3e`) and Signal-Android (`e9b569ed`, 8.32.1). Every check passed.
Evidence is in [testbox-media-traffic-2026-11-19/](testbox-media-traffic-2026-11-19/): `cdn-events.txt` (every
CDN request), `compare.md` (the rerun's comparison, decoded with the fixes below; its Attachments table is the run's numbers), `b33p-fetched.txt` (b33p's download log lines).

The phase: Desktop (linked to the peer) sends the photo and a small PNG in one message to b33p's account. b33p and
Android (b33p's primary) receive and download them. b33p then sends the photo: Desktop downloads it, and Android
downloads it as a sync. Next b33p restarts and shows the chat again. Last, b33p opens Desktop's photo in its viewer.

## What the wire shows

| | Desktop | Android | b33p |
|---|---|---|---|
| The 7.3 MB photo, as uploaded | **155 KB** (scaled to 1600 px, JPEG q0.7) | (same policy, from source) | **8.49 MB** (as is) |
| Blurhash in the pointer | yes | yes | no |
| `GET /attachments/*` in the phase | 3 | 3 | **9** |
| Downloads its own sent picture again | no | no | **yes, 8.49 MB right after the upload** |
| Restart, then the chat shown again | 0 bytes | 0 bytes | **all three again, 8.7 MB** |
| Full-size viewer | local file | local file | **another download** |
| When it downloads | on receipt (queue, 6 at once) | on receipt (a job each) | when the row shows (4 at once) |
| Where a download goes | disk, re-encrypted | disk, re-encrypted | memory only (160 MiB, then evicted) |
| Range / resume | yes | yes | no; buffered into a `Vec` |
| HTTP to the CDN | 1.1 keep-alive | 1.1, pool of 5 for 45 s | 1.1, idle 20 s |

On the box's loopback each transfer takes milliseconds, so this run shows the bytes and not the time a real
connection would need for them. A user's b33p fetches 8.5 MB per phone photo every time a chat with that photo
shows after a start (and again after eviction past 160 MiB), plus once more for the viewer and once more to save
it. Desktop and Android fetch it once per install.

## What b33p's own log shows: decoding

b33p decodes the whole picture for the bubble (`decode_picture`, `image.resize(.., Triangle)` to 640 px) and
does so one picture at a time (`Downloads::decoding`, one permit). In the box (debug build, busy host) the 8.17 MB
photo was ready about **7 s** after its bytes arrived, at both of its shows. The same decode, timed alone:

| build | decode | resize to 640×480 |
|---|---|---|
| release (`opt-level = "s"`) | 83 ms | 295 ms |
| dev (`image` not optimized) | 3.4 s | 1.3 s |

Every picture in the chat waits behind this, and it is repeated on each show, since nothing decoded is kept on disk.
Desktop draws 150 px thumbnails it made once (grids) or lets Chromium decode lazily; Android's Glide keeps the
bubble-sized bitmap in an encrypted disk cache.

## What would close the gap, by effect

1. **Keep downloads on disk** (encrypted, like the store) and show from there: removes the restart, eviction, viewer and
   save downloads. Keep the bytes of what b33p sends, so it never downloads its own upload.
2. **Scale pictures before sending**, as both apps do by default: 1600 px, ≤1 MiB, JPEG q≈0.7 (Desktop
   `scaleImageToLevel.preload.ts`, Android `PushMediaConstraints.java` LEVEL_1). Every receiver and every linked
   device downloads and decodes what b33p uploads, so this matters on their side as well.
3. **Keep a bubble-sized thumbnail on disk**, and decode it cheaply: let the JPEG decoder downscale (DCT scaling)
   or use `fast_image_resize` (already a dependency) rather than `Triangle` on 12 MP.
4. **Send a blurhash**, and draw the received one with width/height until the picture is there.
5. Fetch incoming pictures on receipt, newest/visible first, rather than only when shown; resume with `Range`, and
   stream to disk rather than into memory.
6. Profile `image` (or its JPEG decoder) at opt-level 3 in dev, as for `fast_image_resize`, so the box and
   debug builds don't show 10× slower pictures than users get.

## The box itself

- The first run failed while building the Android image. Another worktree's box was building its own Android image
  at the same moment, and both builds share buildah's cache mount for `/root/.gradle`. Each Gradle is PID 44 in its
  own container, so the lock Gradle waited on looked like its own: "Timeout waiting to lock journal cache …
  Owner PID: 44, Our PID: 44". The rerun after that build finished passed.
- Two connections didn't decode wholly, for two different reasons. The tus server's log shows both requests
  succeeded (PATCH 204, and four GETs of b33p's photo).
  - **b33p's upload: the capture dropped segments.** 83,632 bytes were missing from `raw/capture.pcap`, with no
    duplicates: real holes, behind `compare.md`'s "request cut short". tcpdump ran with the default kernel buffer.
    With `-B 65536` (`testbox/capture/pod.yaml`), a rerun had no holes in any stream, and tcpdump reported
    "0 packets dropped by kernel".
  - **Desktop's download of b33p's photo: the decoder.** "A record of Received didn't open" came again in the
    rerun, though the stream had no holes. It had retransmitted duplicates: 55 KB in this run, 1 MB in the rerun.
    `tls.rs` sorted each side's records by the time their bytes came. A segment held early, until a retransmission
    filled the gap before it, kept its earlier time, so its record sorted ahead of the one before it, and TLS
    1.3's counted nonces failed from there. Records now keep their order within a side. Both runs' Desktop
    downloads decode: 8,489,920 bytes.
  - Open, not related to these checks: 4–5 connections at the end of each record have "no secrets for the
    session". They open 11–24 s after the last decoded event, while the record is taken (Signal-Server is
    restarted for its log), so their secrets are likely missing from the key logs copied then. Not verified.
