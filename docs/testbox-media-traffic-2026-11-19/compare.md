# What the clients said to the servers

From `--capture`'s tap. `=>` a client's request, `<=` the server's (the chat's deliveries). Paths as templates (xtask/src/wire/template.rs).

## Connections

| client | connections | ports | ALPN | TLS hellos | longest (s) | not decoded |
|---|---|---|---|---|---|---|
| android | 28 | 8443, 8445, 8446, 8447, 8448 | -, h2, http/1.1 | ceb40329, e3d12203 | 136 | 0 |
| b33p | 22 | 8444, 8446, 8448 | -, http/1.1 | e60eff0e | 93 | 0 |
| desktop | 13 | 8443, 8445, 8446, 8447, 8448 | -, h2 | 433d3953, ceb40329 | 164 | 0 |
| presage-cli | 8 | 8444, 8448 | - | e60eff0e | 0 | 0 |
| presage-rs | 6 | 8444, 8446 | -, http/1.1 | e60eff0e | 1 | 0 |

31 more connections carried no byte either way.

## Requests

How many of each request each client made, and the port where it isn't Signal-Server's own. **Bold**: only one client made it.

| request | android | b33p | desktop | presage-cli | presage-rs | unknown |
|---|---|---|---|---|---|---|
| `<= PUT /api/v1/message` | 9 | 11 | 11 | 6 |  |  |
| `<= PUT /api/v1/queue/empty` | 2 | 2 | 2 | 3 |  |  |
| `<= PUT /v1/address` |  | 1 | 1 |  |  |  |
| `<= PUT /v1/message` |  | 1 | 1 |  |  |  |
| `CONNECT /v1/websocket/` | 6 |  | 4 |  |  |  |
| **`CONNECT /v1/websocket/provisioning/`** |  |  | 1 |  |  |  |
| **`CONNECT /v1/{id}/discovery`** | 3 |  |  |  |  |  |
| `GET /attachments/{id}` | 3 (8446) | 9 (8446) | 2 (8446) |  |  |  |
| `GET /stickers/{id}/manifest.proto` | 11 (8447) |  | 11 (8447) |  |  |  |
| `GET /v1/accounts/whoami` | 2 |  |  | 1 |  |  |
| **`GET /v1/certificate/auth/group?redemptionEndSeconds={v}&redemptionStartSeconds={v}&v101={v}&zkcCredential={v}`** |  |  | 1 |  |  |  |
| **`GET /v1/certificate/delivery`** | 1 |  |  |  |  |  |
| `GET /v1/certificate/delivery?includeE164={v}` | 1 | 1 | 1 | 1 |  |  |
| `GET /v1/devices/provisioning/code` | 1 |  |  | 1 |  |  |
| **`GET /v1/devices/transfer_archive?timeout={v}`** |  | 1 |  |  |  |  |
| **`GET /v1/devices/wait_for_linked_device/{id}?timeout={v}`** | 1 |  |  |  |  |  |
| `GET /v1/keepalive` | 6 | 5 | 6 |  |  |  |
| `GET /v1/profile/{aci}/{id}` | 1 | 4 | 1 | 2 |  |  |
| `GET /v1/profile/{aci}/{id}/{id}?credentialType={v}` | 2 |  | 2 |  |  |  |
| `GET /v1/storage/auth` | 10 | 10 | 4 | 2 |  |  |
| `GET /v1/storage/manifest` | 2 (8448) | 4 (8448) | 2 (8448) | 2 (8448) |  |  |
| `GET /v1/storage/manifest/version/{n}` | 1 (8448) | 6 (8448) | 2 (8448) |  |  |  |
| `GET /v1/websocket/` |  | 4 |  | 4 | 5 |  |
| **`GET /v1/websocket/provisioning/`** |  | 1 |  |  |  |  |
| **`GET /v2/config`** |  |  | 2 |  |  |  |
| **`GET /v2/directory/auth`** | 3 |  |  |  |  |  |
| `GET /v2/keys/{aci}/*` |  | 1 | 2 | 1 |  |  |
| `GET /v2/keys/{aci}/{n}` |  | 1 |  | 1 |  |  |
| `GET /v2/keys?identity={v}` | 6 | 4 | 2 | 3 |  |  |
| `GET /v4/attachments/form/upload?uploadLength={v}` |  | 1 | 2 | 1 |  |  |
| **`HEAD /dynamic/release-notes/release-notes-v2.json`** |  |  | 1 |  |  |  |
| `HEAD /upload/attachments/{id}` | 1 (8446) | 1 (8446) |  |  | 1 (8446) |  |
| `PATCH /upload/attachments/{id}` | 1 (8446) | 1 (8446) |  |  | 1 (8446) |  |
| `PATCH /v1/verification/session/{id}` | 1 |  |  |  | 1 |  |
| **`POST /org.signal.chat.attachments.Attachments/GetUploadForm`** | 2 |  |  |  |  |  |
| `POST /org.signal.chat.device.Devices/GetDevices` | 2 |  | 1 |  |  |  |
| `POST /upload/attachments` | 2 (8446) | 1 (8446) | 2 (8446) |  | 1 (8446) |  |
| `POST /v1/registration` | 1 |  |  |  | 1 |  |
| `POST /v1/verification/session` | 1 |  |  |  | 1 |  |
| `POST /v1/verification/session/{id}/code` | 1 |  |  |  | 1 |  |
| `POST /v2/keys/check` |  | 4 |  | 2 |  |  |
| `PUT /v1/accounts/attributes` | 5 |  |  | 3 |  |  |
| **`PUT /v1/archives/backupid`** | 1 |  |  |  |  |  |
| `PUT /v1/devices/capabilities` |  | 2 | 1 |  |  |  |
| `PUT /v1/devices/link` |  | 1 | 1 |  |  |  |
| **`PUT /v1/devices/transfer_archive`** | 1 |  |  |  |  |  |
| `PUT /v1/messages/{aci}` |  | 11 |  | 5 |  |  |
| `PUT /v1/messages/{aci}?story={v}` | 10 |  | 11 |  |  |  |
| `PUT /v1/profile` | 1 |  |  | 1 |  |  |
| `PUT /v1/provisioning/{id}` | 1 |  |  | 1 |  |  |
| **`PUT /v1/storage/`** |  |  | 3 (8448) |  |  |  |
| **`PUT /v1/storage/read`** |  | 3 (8448) |  |  |  |  |
| **`PUT /v1/storage?sendRemoteManifestOnConflict={v}`** | 7 (8448) |  |  |  |  |  |
| `PUT /v1/verification/session/{id}/code` | 1 |  |  |  | 1 |  |
| `PUT /v2/keys?identity={v}` | 2 | 2 | 2 | 1 |  |  |

## Keeping the chat alive

| client | GET /v1/keepalive | its interval (s, median) | WebSocket pings | HTTP/2 pings |
|---|---|---|---|---|
| android | 6 | 30.0 | 3 | 0 |
| b33p | 5 | 30.0 | 0 | 0 |
| desktop | 6 | 30.0 | 0 | 0 |
| presage-cli | 0 | - | 0 | 0 |
| presage-rs | 0 | - | 0 | 0 |
| unknown | 0 | - | 0 | 0 |

HTTP/2 settings:

- android (client): `SETTINGS ENABLE_PUSH=0 INITIAL_WINDOW_SIZE=2097152 MAX_FRAME_SIZE=16384 MAX_HEADER_LIST_SIZE=16384`
- android (client): `SETTINGS INITIAL_WINDOW_SIZE=16777216`
- android (client): `SETTINGS ack`
- android (server): `SETTINGS MAX_CONCURRENT_STREAMS=100 MAX_HEADER_LIST_SIZE=8192 ENABLE_CONNECT_PROTOCOL=1`
- android (server): `SETTINGS MAX_CONCURRENT_STREAMS=1024 MAX_HEADER_LIST_SIZE=8192 ENABLE_CONNECT_PROTOCOL=1`
- android (server): `SETTINGS ack`
- desktop (client): `SETTINGS ENABLE_PUSH=0 INITIAL_WINDOW_SIZE=2097152 MAX_FRAME_SIZE=16384 MAX_HEADER_LIST_SIZE=16384`
- desktop (client): `SETTINGS ack`
- desktop (server): `SETTINGS MAX_CONCURRENT_STREAMS=100 MAX_HEADER_LIST_SIZE=8192 ENABLE_CONNECT_PROTOCOL=1`
- desktop (server): `SETTINGS ack`

## The same request, made differently

- `<= PUT /api/v1/message`
  - body:
    - android: `envelope clientTimestamp content destinationServiceIdBinary serverGuidBinary serverTimestamp sourceDeviceId sourceServiceIdBinary type=1 urgent` / `envelope clientTimestamp content destinationServiceIdBinary serverGuidBinary serverTimestamp sourceDeviceId sourceServiceIdBinary type=3 urgent` / `envelope clientTimestamp content destinationServiceIdBinary serverGuidBinary serverTimestamp type=6 urgent`
    - b33p: `envelope clientTimestamp content destinationServiceIdBinary serverGuidBinary serverTimestamp sourceDeviceId sourceServiceIdBinary type=1 urgent` / `envelope clientTimestamp content destinationServiceIdBinary serverGuidBinary serverTimestamp sourceDeviceId sourceServiceIdBinary type=3 urgent` / `envelope clientTimestamp content destinationServiceIdBinary serverGuidBinary serverTimestamp type=6 urgent`
    - desktop: `envelope clientTimestamp content destinationServiceIdBinary serverGuidBinary serverTimestamp sourceDeviceId sourceServiceIdBinary type=1 urgent` / `envelope clientTimestamp content destinationServiceIdBinary serverGuidBinary serverTimestamp type=6 urgent` / `envelope clientTimestamp destinationServiceIdBinary serverGuidBinary serverTimestamp sourceDeviceId sourceServiceIdBinary type=5 urgent`
    - presage-cli: `envelope clientTimestamp content destinationServiceIdBinary serverGuidBinary serverTimestamp sourceDeviceId sourceServiceIdBinary type=3 urgent`
- `<= PUT /api/v1/queue/empty`
  - answer:
    - android: `-`
    - b33p: `200 empty`
    - desktop: `-`
    - presage-cli: `200 empty`
- `<= PUT /v1/message`
  - answer:
    - b33p: `200 empty`
    - desktop: `-`
- `CONNECT /v1/websocket/`
  - accept-language:
    - android: (none)
    - desktop: `en`
  - headers:
    - android: `authorization sec-websocket-version user-agent x-signal-receive-stories` / `sec-websocket-version user-agent`
    - desktop: `accept-language authorization sec-websocket-version user-agent x-signal-receive-stories` / `accept-language sec-websocket-version user-agent`
  - user-agent:
    - android: `Signal-Android/8.32.1 Android/35 libsignal/0.105.0`
    - desktop: `Signal-Desktop/44.7.2 Linux 7.3.4-1-cachyos libsignal/0.105.0`
- `GET /attachments/{id}`
  - accept-encoding:
    - android: `gzip`
    - b33p: (none)
    - desktop: `gzip, deflate, br`
  - accept-language:
    - android: (none)
    - b33p: `en`
    - desktop: (none)
  - headers:
    - android: `accept-encoding connection host user-agent`
    - b33p: `accept accept-language host user-agent`
    - desktop: `accept accept-encoding connection host user-agent x-signal-agent`
  - user-agent:
    - android: `Signal-Android/8.32.1 Android/35`
    - b33p: `b33p/0.2.0 presage-rs`
    - desktop: `Signal-Desktop/44.7.2 Linux 7.3.4-1-cachyos`
  - x-signal-agent:
    - android: (none)
    - b33p: (none)
    - desktop: `OWD`
- `GET /stickers/{id}/manifest.proto`
  - accept-encoding:
    - android: `gzip`
    - desktop: `gzip, deflate, br`
  - headers:
    - android: `accept-encoding connection host user-agent`
    - desktop: `accept accept-encoding connection host user-agent x-signal-agent`
  - user-agent:
    - android: `Signal-Android/8.32.1 Android/35`
    - desktop: `Signal-Desktop/44.7.2 Linux 7.3.4-1-cachyos`
  - x-signal-agent:
    - android: (none)
    - desktop: `OWD`
- `GET /v1/certificate/delivery?includeE164={v}`
  - content-type:
    - android: (none)
    - b33p: (none)
    - desktop: `application/json; charset=utf-8`
    - presage-cli: (none)
  - headers:
    - android: (empty)
    - b33p: (empty)
    - desktop: `authorization content-type x-signal-agent`
    - presage-cli: (empty)
  - x-signal-agent:
    - android: (none)
    - b33p: (none)
    - desktop: `OWD`
    - presage-cli: (none)
- `GET /v1/devices/provisioning/code`
  - headers:
    - android: (empty)
    - presage-cli: `accept authorization host user-agent`
  - user-agent:
    - android: (none)
    - presage-cli: `presage-rs-0.8.0-dev`
- `GET /v1/profile/{aci}/{id}`
  - accept-language:
    - android: `en;q=1`
    - b33p: (none)
    - desktop: `en`
    - presage-cli: (none)
  - answer:
    - android: `200 json about:string aboutEmoji:string avatar:null badges:array capabilities.attachmentBackfill:bool capabilities.profiles_v2:bool capabilities.spqr:bool capabilities.usernameChangeSyncMessage:bool capabilities:object identityKey:string name:string paymentAddress:null phoneNumberSharing:string unidentifiedAccess:string unrestrictedUnidentifiedAccess:bool uuid:string`
    - b33p: `200 json about:string aboutEmoji:string avatar:null badges:array capabilities.attachmentBackfill:bool capabilities.profiles_v2:bool capabilities.spqr:bool capabilities.usernameChangeSyncMessage:bool capabilities:object identityKey:string name:string paymentAddress:null phoneNumberSharing:null unidentifiedAccess:string unrestrictedUnidentifiedAccess:bool uuid:string` / `200 json about:string aboutEmoji:string avatar:null badges:array capabilities.attachmentBackfill:bool capabilities.profiles_v2:bool capabilities.spqr:bool capabilities.usernameChangeSyncMessage:bool capabilities:object identityKey:string name:string paymentAddress:null phoneNumberSharing:string unidentifiedAccess:string unrestrictedUnidentifiedAccess:bool uuid:string`
    - desktop: `200 json about:string aboutEmoji:string avatar:null badges:array capabilities.attachmentBackfill:bool capabilities.profiles_v2:bool capabilities.spqr:bool capabilities.usernameChangeSyncMessage:bool capabilities:object identityKey:string name:string paymentAddress:null phoneNumberSharing:string unidentifiedAccess:string unrestrictedUnidentifiedAccess:bool uuid:string`
    - presage-cli: `-` / `200 json about:string aboutEmoji:string avatar:null badges:array capabilities.attachmentBackfill:bool capabilities.profiles_v2:bool capabilities.spqr:bool capabilities.usernameChangeSyncMessage:bool capabilities:object identityKey:string name:string paymentAddress:null phoneNumberSharing:null unidentifiedAccess:string unrestrictedUnidentifiedAccess:bool uuid:string`
  - content-type:
    - android: (none)
    - b33p: (none)
    - desktop: `application/json; charset=utf-8`
    - presage-cli: (none)
  - headers:
    - android: `accept-language`
    - b33p: (empty)
    - desktop: `accept-language content-type unidentified-access-key x-signal-agent`
    - presage-cli: (empty)
  - x-signal-agent:
    - android: (none)
    - b33p: (none)
    - desktop: `OWD`
    - presage-cli: (none)
- `GET /v1/profile/{aci}/{id}/{id}?credentialType={v}`
  - accept-language:
    - android: `en;q=1`
    - desktop: `en`
  - content-type:
    - android: (none)
    - desktop: `application/json; charset=utf-8`
  - headers:
    - android: `accept-language` / `unidentified-access-key`
    - desktop: `accept-language authorization content-type x-signal-agent` / `accept-language content-type unidentified-access-key x-signal-agent`
  - x-signal-agent:
    - android: (none)
    - desktop: `OWD`
- `GET /v1/storage/auth`
  - accept-language:
    - android: (none)
    - b33p: `en`
    - desktop: (none)
    - presage-cli: (none)
  - content-type:
    - android: (none)
    - b33p: (none)
    - desktop: `application/json; charset=utf-8`
    - presage-cli: (none)
  - headers:
    - android: (empty)
    - b33p: `accept accept-language authorization host user-agent`
    - desktop: `authorization content-type x-signal-agent`
    - presage-cli: `accept authorization host user-agent`
  - user-agent:
    - android: (none)
    - b33p: `b33p/0.2.0 presage-rs`
    - desktop: (none)
    - presage-cli: `presage-rs-0.8.0-dev`
  - x-signal-agent:
    - android: (none)
    - b33p: (none)
    - desktop: `OWD`
    - presage-cli: (none)
- `GET /v1/storage/manifest`
  - accept-encoding:
    - android: `gzip`
    - b33p: (none)
    - desktop: `gzip, deflate, br`
    - presage-cli: (none)
  - accept-language:
    - android: (none)
    - b33p: `en`
    - desktop: (none)
    - presage-cli: (none)
  - answer:
    - android: `200 protobuf 1:varint 2:bytes` / `404 json code:number message:string`
    - b33p: `200 protobuf 1:varint 2:bytes`
    - desktop: `200 protobuf 2:bytes` / `404 json code:number message:string`
    - presage-cli: `200 protobuf 1:varint 2:bytes`
  - content-type:
    - android: (none)
    - b33p: (none)
    - desktop: `application/x-protobuf`
    - presage-cli: (none)
  - headers:
    - android: `accept-encoding authorization connection host user-agent`
    - b33p: `accept accept-language authorization host user-agent`
    - desktop: `accept accept-encoding authorization connection content-type host user-agent x-signal-agent`
    - presage-cli: `accept authorization host user-agent`
  - user-agent:
    - android: `Signal-Android/8.32.1 Android/35`
    - b33p: `b33p/0.2.0 presage-rs`
    - desktop: `Signal-Desktop/44.7.2 Linux 7.3.4-1-cachyos`
    - presage-cli: `presage-rs-0.8.0-dev`
  - x-signal-agent:
    - android: (none)
    - b33p: (none)
    - desktop: `OWD`
    - presage-cli: (none)
- `GET /v1/storage/manifest/version/{n}`
  - accept-encoding:
    - android: `gzip`
    - b33p: (none)
    - desktop: `gzip, deflate, br`
  - accept-language:
    - android: (none)
    - b33p: `en`
    - desktop: (none)
  - answer:
    - android: `204 empty`
    - b33p: `200 protobuf 1:varint 2:bytes` / `204 empty`
    - desktop: `204 empty`
  - content-type:
    - android: (none)
    - b33p: (none)
    - desktop: `application/x-protobuf`
  - headers:
    - android: `accept-encoding authorization connection host user-agent`
    - b33p: `accept accept-language authorization host user-agent`
    - desktop: `accept accept-encoding authorization connection content-type host user-agent x-signal-agent`
  - user-agent:
    - android: `Signal-Android/8.32.1 Android/35`
    - b33p: `b33p/0.2.0 presage-rs`
    - desktop: `Signal-Desktop/44.7.2 Linux 7.3.4-1-cachyos`
  - x-signal-agent:
    - android: (none)
    - b33p: (none)
    - desktop: `OWD`
- `GET /v1/websocket/`
  - accept-language:
    - b33p: `en`
    - presage-cli: (none)
    - presage-rs: (none)
  - answer:
    - b33p: `101 empty`
    - presage-cli: `101 empty`
    - presage-rs: `-` / `101 empty`
  - headers:
    - b33p: `accept accept-language authorization connection host sec-websocket-key sec-websocket-version upgrade user-agent x-signal-receive-stories` / `accept accept-language connection host sec-websocket-key sec-websocket-version upgrade user-agent`
    - presage-cli: `accept authorization connection host sec-websocket-key sec-websocket-version upgrade user-agent x-signal-receive-stories`
    - presage-rs: `accept connection host sec-websocket-key sec-websocket-version upgrade user-agent`
  - user-agent:
    - b33p: `b33p/0.2.0 presage-rs`
    - presage-cli: `presage-rs-0.8.0-dev`
    - presage-rs: `presage-rs-0.8.0-dev`
  - x-signal-receive-stories:
    - b33p: `true`
    - presage-cli: `false`
    - presage-rs: (none)
- `GET /v2/keys/{aci}/*`
  - answer:
    - b33p: `200 json devices:array devices[].deviceId:number devices[].pqPreKey.keyId:number devices[].pqPreKey.publicKey:string devices[].pqPreKey.signature:string devices[].pqPreKey:object devices[].preKey.keyId:number devices[].preKey.publicKey:string devices[].preKey:null devices[].preKey:object devices[].registrationId:number devices[].signedPreKey.keyId:number devices[].signedPreKey.publicKey:string devices[].signedPreKey.signature:string devices[].signedPreKey:object devices[]:object identityKey:string`
    - desktop: `200 json devices:array devices[].deviceId:number devices[].pqPreKey.keyId:number devices[].pqPreKey.publicKey:string devices[].pqPreKey.signature:string devices[].pqPreKey:object devices[].preKey.keyId:number devices[].preKey.publicKey:string devices[].preKey:null devices[].preKey:object devices[].registrationId:number devices[].signedPreKey.keyId:number devices[].signedPreKey.publicKey:string devices[].signedPreKey.signature:string devices[].signedPreKey:object devices[]:object identityKey:string` / `200 json devices:array devices[].deviceId:number devices[].pqPreKey.keyId:number devices[].pqPreKey.publicKey:string devices[].pqPreKey.signature:string devices[].pqPreKey:object devices[].preKey.keyId:number devices[].preKey.publicKey:string devices[].preKey:object devices[].registrationId:number devices[].signedPreKey.keyId:number devices[].signedPreKey.publicKey:string devices[].signedPreKey.signature:string devices[].signedPreKey:object devices[]:object identityKey:string`
    - presage-cli: `200 json devices:array devices[].deviceId:number devices[].pqPreKey.keyId:number devices[].pqPreKey.publicKey:string devices[].pqPreKey.signature:string devices[].pqPreKey:object devices[].preKey.keyId:number devices[].preKey.publicKey:string devices[].preKey:object devices[].registrationId:number devices[].signedPreKey.keyId:number devices[].signedPreKey.publicKey:string devices[].signedPreKey.signature:string devices[].signedPreKey:object devices[]:object identityKey:string`
  - content-type:
    - b33p: (none)
    - desktop: `application/json; charset=utf-8`
    - presage-cli: (none)
  - headers:
    - b33p: (empty)
    - desktop: `authorization content-type x-signal-agent` / `content-type unidentified-access-key x-signal-agent`
    - presage-cli: (empty)
  - x-signal-agent:
    - b33p: (none)
    - desktop: `OWD`
    - presage-cli: (none)
- `GET /v2/keys?identity={v}`
  - content-type:
    - android: (none)
    - b33p: (none)
    - desktop: `application/json; charset=utf-8`
    - presage-cli: (none)
  - headers:
    - android: (empty)
    - b33p: (empty)
    - desktop: `authorization content-type x-signal-agent`
    - presage-cli: (empty)
  - x-signal-agent:
    - android: (none)
    - b33p: (none)
    - desktop: `OWD`
    - presage-cli: (none)
- `GET /v4/attachments/form/upload?uploadLength={v}`
  - accept-language:
    - b33p: `en`
    - desktop: (none)
    - presage-cli: (none)
  - headers:
    - b33p: `accept accept-language authorization host user-agent`
    - desktop: (empty)
    - presage-cli: `accept authorization host user-agent`
  - user-agent:
    - b33p: `b33p/0.2.0 presage-rs`
    - desktop: (none)
    - presage-cli: `presage-rs-0.8.0-dev`
- `HEAD /upload/attachments/{id}`
  - accept-encoding:
    - android: `gzip`
    - b33p: (none)
    - presage-rs: (none)
  - accept-language:
    - android: (none)
    - b33p: `en`
    - presage-rs: (none)
  - headers:
    - android: `accept-encoding authorization connection host tus-resumable upload-metadata user-agent x-signal-agent`
    - b33p: `accept accept-language authorization host tus-resumable upload-metadata user-agent`
    - presage-rs: `accept authorization host tus-resumable upload-metadata user-agent`
  - user-agent:
    - android: `Signal-Android/8.32.1 Android/35`
    - b33p: `b33p/0.2.0 presage-rs`
    - presage-rs: `presage-rs-0.8.0-dev`
  - x-signal-agent:
    - android: `OWA`
    - b33p: (none)
    - presage-rs: (none)
- `PATCH /upload/attachments/{id}`
  - accept-encoding:
    - android: `gzip`
    - b33p: (none)
    - presage-rs: (none)
  - accept-language:
    - android: (none)
    - b33p: `en`
    - presage-rs: (none)
  - headers:
    - android: `accept-encoding authorization connection content-length content-type host tus-resumable upload-length upload-metadata upload-offset user-agent x-signal-agent`
    - b33p: `accept accept-language authorization content-length content-type host tus-resumable upload-length upload-metadata upload-offset user-agent`
    - presage-rs: `accept authorization content-length content-type host tus-resumable upload-length upload-metadata upload-offset user-agent`
  - user-agent:
    - android: `Signal-Android/8.32.1 Android/35`
    - b33p: `b33p/0.2.0 presage-rs`
    - presage-rs: `presage-rs-0.8.0-dev`
  - x-signal-agent:
    - android: `OWA`
    - b33p: (none)
    - presage-rs: (none)
- `PATCH /v1/verification/session/{id}`
  - accept-encoding:
    - android: `gzip`
    - presage-rs: (none)
  - content-type:
    - android: `application/json; charset=utf-8`
    - presage-rs: `application/json`
  - headers:
    - android: `accept-encoding content-length content-type user-agent x-signal-agent`
    - presage-rs: `content-type`
  - user-agent:
    - android: `Signal-Android/8.32.1 Android/35`
    - presage-rs: (none)
  - x-signal-agent:
    - android: `OWA`
    - presage-rs: (none)
- `POST /org.signal.chat.device.Devices/GetDevices`
  - accept-language:
    - android: (none)
    - desktop: `en`
  - answer:
    - android: `0 protobuf 1.1:varint 1.3:varint 1.4:varint 1.5:bytes 1:message`
    - desktop: `0 protobuf 1.1:varint 1.2.1:bytes 1.2.2:bytes 1.2.3:bytes 1.2:message 1.3:varint 1.4:varint 1.5:bytes 1:message`
  - headers:
    - android: `authorization content-type te user-agent`
    - desktop: `accept-language authorization content-type te user-agent`
  - user-agent:
    - android: `Signal-Android/8.32.1 Android/35 libsignal/0.105.0`
    - desktop: `Signal-Desktop/44.7.2 Linux 7.3.4-1-cachyos libsignal/0.105.0`
- `POST /upload/attachments`
  - accept-encoding:
    - android: `gzip`
    - b33p: (none)
    - desktop: `gzip, deflate, br`
    - presage-rs: (none)
  - accept-language:
    - android: (none)
    - b33p: `en`
    - desktop: (none)
    - presage-rs: (none)
  - answer:
    - android: `201 binary`
    - b33p: `201 empty`
    - desktop: `201 binary`
    - presage-rs: `201 empty`
  - body:
    - android: `binary` / `empty`
    - b33p: `empty`
    - desktop: `binary`
    - presage-rs: `empty`
  - content-type:
    - android: `application/offset+octet-stream`
    - b33p: (none)
    - desktop: `application/offset+octet-stream`
    - presage-rs: (none)
  - headers:
    - android: `accept-encoding authorization connection content-length content-type host tus-resumable upload-length upload-metadata user-agent x-signal-agent x-signal-checksum-sha256` / `accept-encoding authorization connection content-length host tus-resumable upload-defer-length upload-metadata user-agent x-signal-agent`
    - b33p: `accept accept-language authorization content-length host tus-resumable upload-defer-length upload-metadata user-agent`
    - desktop: `accept accept-encoding authorization connection content-type host transfer-encoding tus-resumable upload-length upload-metadata user-agent x-signal-agent`
    - presage-rs: `accept authorization content-length host tus-resumable upload-defer-length upload-metadata user-agent`
  - user-agent:
    - android: `Signal-Android/8.32.1 Android/35`
    - b33p: `b33p/0.2.0 presage-rs`
    - desktop: `Signal-Desktop/44.7.2 Linux 7.3.4-1-cachyos`
    - presage-rs: `presage-rs-0.8.0-dev`
  - x-signal-agent:
    - android: `OWA`
    - b33p: (none)
    - desktop: `OWD`
    - presage-rs: (none)
- `POST /v1/registration`
  - accept-encoding:
    - android: `gzip`
    - presage-rs: (none)
  - answer:
    - android: `200 binary`
    - presage-rs: `200 json authCredentialSalt:null entitlements.backup:null entitlements.badges:array entitlements:object number:string pni:string reregistration:bool storageCapable:bool usernameHash:null usernameLinkHandle:null uuid:string`
  - body:
    - android: `json accountAttributes.capabilities.attachmentBackfill:bool accountAttributes.capabilities.optionalPhoneNumber:bool accountAttributes.capabilities.spqr:bool accountAttributes.capabilities.storage:bool accountAttributes.capabilities.usernameChangeSyncMessage:bool accountAttributes.capabilities.versionedExpirationTimer:bool accountAttributes.capabilities:object accountAttributes.discoverableByPhoneNumber:bool accountAttributes.fetchesMessages:bool accountAttributes.name:null accountAttributes.pniRegistrationId:number accountAttributes.recoveryPassword:string accountAttributes.registrationId:number accountAttributes.registrationLock:null accountAttributes.signalingKey:null accountAttributes.unidentifiedAccessKey:string accountAttributes.unrestrictedUnidentifiedAccess:bool accountAttributes.video:bool accountAttributes.voice:bool accountAttributes:object aciIdentityKey:string aciPqLastResortPreKey.keyId:number aciPqLastResortPreKey.publicKey:string aciPqLastResortPreKey.signature:string aciPqLastResortPreKey:object aciSignedPreKey.keyId:number aciSignedPreKey.publicKey:string aciSignedPreKey.signature:string aciSignedPreKey:object gcmToken:null pniIdentityKey:string pniPqLastResortPreKey.keyId:number pniPqLastResortPreKey.publicKey:string pniPqLastResortPreKey.signature:string pniPqLastResortPreKey:object pniSignedPreKey.keyId:number pniSignedPreKey.publicKey:string pniSignedPreKey.signature:string pniSignedPreKey:object receiptCredentialPresentation:null recoveryPassword:null requireAtomic:bool sessionId:string skipDeviceTransfer:bool totp:null webAuthnResponse:null`
    - presage-rs: `json accountAttributes.capabilities.attachmentBackfill:bool accountAttributes.capabilities.profiles_v2:bool accountAttributes.capabilities.spqr:bool accountAttributes.capabilities.storage:bool accountAttributes.capabilities.transfer:bool accountAttributes.capabilities.usernameChangeSyncMessage:bool accountAttributes.capabilities:object accountAttributes.discoverableByPhoneNumber:bool accountAttributes.fetchesMessages:bool accountAttributes.name:null accountAttributes.pniRegistrationId:number accountAttributes.registrationId:number accountAttributes.unidentifiedAccessKey:string accountAttributes.unrestrictedUnidentifiedAccess:bool accountAttributes.video:bool accountAttributes.voice:bool accountAttributes:object aciIdentityKey:string aciPqLastResortPreKey.keyId:number aciPqLastResortPreKey.publicKey:string aciPqLastResortPreKey.signature:string aciPqLastResortPreKey:object aciSignedPreKey.keyId:number aciSignedPreKey.publicKey:string aciSignedPreKey.signature:string aciSignedPreKey:object gcmToken:null pniIdentityKey:string pniPqLastResortPreKey.keyId:number pniPqLastResortPreKey.publicKey:string pniPqLastResortPreKey.signature:string pniPqLastResortPreKey:object pniSignedPreKey.keyId:number pniSignedPreKey.publicKey:string pniSignedPreKey.signature:string pniSignedPreKey:object recoveryPassword:null requireAtomic:bool sessionId:string skipDeviceTransfer:bool`
  - content-type:
    - android: `application/json; charset=utf-8`
    - presage-rs: `application/json`
  - headers:
    - android: `accept-encoding authorization content-length content-type user-agent x-signal-agent`
    - presage-rs: `authorization content-type`
  - user-agent:
    - android: `Signal-Android/8.32.1 Android/35`
    - presage-rs: (none)
  - x-signal-agent:
    - android: `OWA`
    - presage-rs: (none)
- `POST /v1/verification/session`
  - accept-encoding:
    - android: `gzip`
    - presage-rs: (none)
  - content-type:
    - android: `application/json; charset=utf-8`
    - presage-rs: `application/json`
  - headers:
    - android: `accept-encoding content-length content-type user-agent x-signal-agent`
    - presage-rs: `content-type`
  - user-agent:
    - android: `Signal-Android/8.32.1 Android/35`
    - presage-rs: (none)
  - x-signal-agent:
    - android: `OWA`
    - presage-rs: (none)
- `POST /v1/verification/session/{id}/code`
  - accept-encoding:
    - android: `gzip`
    - presage-rs: (none)
  - accept-language:
    - android: `en-US`
    - presage-rs: (none)
  - content-type:
    - android: `application/json; charset=utf-8`
    - presage-rs: `application/json`
  - headers:
    - android: `accept-encoding accept-language content-length content-type user-agent x-signal-agent`
    - presage-rs: `content-type`
  - user-agent:
    - android: `Signal-Android/8.32.1 Android/35`
    - presage-rs: (none)
  - x-signal-agent:
    - android: `OWA`
    - presage-rs: (none)
- `POST /v2/keys/check`
  - answer:
    - b33p: `200 empty`
    - presage-cli: `-` / `200 empty`
- `PUT /v1/accounts/attributes`
  - body:
    - android: `json capabilities.attachmentBackfill:bool capabilities.optionalPhoneNumber:bool capabilities.spqr:bool capabilities.storage:bool capabilities.usernameChangeSyncMessage:bool capabilities.versionedExpirationTimer:bool capabilities:object discoverableByPhoneNumber:bool fetchesMessages:bool name:null pniRegistrationId:number recoveryPassword:string registrationId:number registrationLock:null signalingKey:null unidentifiedAccessKey:string unrestrictedUnidentifiedAccess:bool video:bool voice:bool`
    - presage-cli: `json capabilities.attachmentBackfill:bool capabilities.profiles_v2:bool capabilities.spqr:bool capabilities.storage:bool capabilities.transfer:bool capabilities.usernameChangeSyncMessage:bool capabilities:object discoverableByPhoneNumber:bool fetchesMessages:bool name:null pniRegistrationId:number registrationId:number unidentifiedAccessKey:string unrestrictedUnidentifiedAccess:bool video:bool voice:bool`
- `PUT /v1/devices/capabilities`
  - body:
    - b33p: `json attachmentBackfill:bool profiles_v2:bool spqr:bool storage:bool transfer:bool usernameChangeSyncMessage:bool`
    - desktop: `json attachmentBackfill:bool optionalPhoneNumber:bool spqr:bool usernameChangeSyncMessage:bool`
  - content-type:
    - b33p: `application/json`
    - desktop: `application/json; charset=utf-8`
  - headers:
    - b33p: `content-type`
    - desktop: `authorization content-type x-signal-agent`
  - x-signal-agent:
    - b33p: (none)
    - desktop: `OWD`
- `PUT /v1/devices/link`
  - accept-language:
    - b33p: `en`
    - desktop: (none)
  - body:
    - b33p: `json accountAttributes.capabilities.attachmentBackfill:bool accountAttributes.capabilities.spqr:bool accountAttributes.capabilities.usernameChangeSyncMessage:bool accountAttributes.capabilities:object accountAttributes.fetchesMessages:bool accountAttributes.name:string accountAttributes.pniRegistrationId:number accountAttributes.registrationId:number accountAttributes:object aciPqLastResortPreKey.keyId:number aciPqLastResortPreKey.publicKey:string aciPqLastResortPreKey.signature:string aciPqLastResortPreKey:object aciSignedPreKey.keyId:number aciSignedPreKey.publicKey:string aciSignedPreKey.signature:string aciSignedPreKey:object pniPqLastResortPreKey.keyId:number pniPqLastResortPreKey.publicKey:string pniPqLastResortPreKey.signature:string pniPqLastResortPreKey:object pniSignedPreKey.keyId:number pniSignedPreKey.publicKey:string pniSignedPreKey.signature:string pniSignedPreKey:object verificationCode:string`
    - desktop: `json accountAttributes.capabilities.attachmentBackfill:bool accountAttributes.capabilities.optionalPhoneNumber:bool accountAttributes.capabilities.spqr:bool accountAttributes.capabilities.usernameChangeSyncMessage:bool accountAttributes.capabilities:object accountAttributes.fetchesMessages:bool accountAttributes.name:string accountAttributes.pniRegistrationId:number accountAttributes.registrationId:number accountAttributes:object aciPqLastResortPreKey.keyId:number aciPqLastResortPreKey.publicKey:string aciPqLastResortPreKey.signature:string aciPqLastResortPreKey:object aciSignedPreKey.keyId:number aciSignedPreKey.publicKey:string aciSignedPreKey.signature:string aciSignedPreKey:object pniPqLastResortPreKey.keyId:number pniPqLastResortPreKey.publicKey:string pniPqLastResortPreKey.signature:string pniPqLastResortPreKey:object pniSignedPreKey.keyId:number pniSignedPreKey.publicKey:string pniSignedPreKey.signature:string pniSignedPreKey:object verificationCode:string`
  - content-type:
    - b33p: `application/json`
    - desktop: `application/json; charset=utf-8`
  - headers:
    - b33p: `accept accept-language authorization content-length content-type host user-agent`
    - desktop: `authorization content-type x-signal-agent`
  - user-agent:
    - b33p: `b33p/0.2.0 presage-rs`
    - desktop: (none)
  - x-signal-agent:
    - b33p: (none)
    - desktop: `OWD`
- `PUT /v1/messages/{aci}`
  - headers:
    - b33p: `content-type` / `content-type unidentified-access-key`
    - presage-cli: `content-type`
  - online:
    - b33p: `false` / `true`
    - presage-cli: `false`
  - timestamp:
    - b33p: `1795089671092` / `1795089671168` / `1795089671198` / `1795089733286` / `1795089733437` / `1795089735817` / `1795089742059` / `1795089742062`
    - presage-cli: `1795089727139` / `1795089727334` / `1795089727473`
- `PUT /v1/messages/{aci}?story={v}`
  - answer:
    - android: `200 json needsSync:bool`
    - desktop: `200 json needsSync:bool` / `409 json extraDevices:array missingDevices:array missingDevices[]:number`
  - body:
    - android: `json destination:string messages:array messages[].content:string messages[].destinationDeviceId:number messages[].destinationRegistrationId:number messages[].type:number messages[]:object online:bool timestamp:number urgent:bool`
    - desktop: `json messages:array messages[].content:string messages[].destinationDeviceId:number messages[].destinationRegistrationId:number messages[].type:number messages[]:object online:bool timestamp:number urgent:bool`
  - content-type:
    - android: `application/json`
    - desktop: `application/json; charset=utf-8`
  - headers:
    - android: `content-type` / `content-type unidentified-access-key`
    - desktop: `authorization content-type x-signal-agent` / `content-type unidentified-access-key x-signal-agent`
  - timestamp:
    - android: `1795089671148` / `1795089671153` / `1795089671223` / `1795089727980` / `1795089730344` / `1795089732063` / `1795089733390` / `1795089735717` / `1795089735850` / `1795089735859`
    - desktop: `1795089606387` / `1795089606414` / `1795089606415` / `1795089607114` / `1795089607617` / `1795089728903` / `1795089734989` / `1795089748440`
  - x-signal-agent:
    - android: (none)
    - desktop: `OWD`
- `PUT /v1/profile`
  - body:
    - android: `json about:string aboutEmoji:string avatar:bool badgeIds:array commitment:string name:string paymentAddress:null phoneNumberSharing:string sameAvatar:bool version:string`
    - presage-cli: `json about:string aboutEmoji:string avatar:bool commitment:string name:string sameAvatar:bool version:string`
- `PUT /v1/provisioning/{id}`
  - headers:
    - android: `content-type`
    - presage-cli: `accept authorization content-length content-type host user-agent`
  - user-agent:
    - android: (none)
    - presage-cli: `presage-rs-0.8.0-dev`
- `PUT /v1/verification/session/{id}/code`
  - accept-encoding:
    - android: `gzip`
    - presage-rs: (none)
  - content-type:
    - android: `application/json; charset=utf-8`
    - presage-rs: `application/json`
  - headers:
    - android: `accept-encoding content-length content-type user-agent x-signal-agent`
    - presage-rs: `content-type`
  - user-agent:
    - android: `Signal-Android/8.32.1 Android/35`
    - presage-rs: (none)
  - x-signal-agent:
    - android: `OWA`
    - presage-rs: (none)
- `PUT /v2/keys?identity={v}`
  - answer:
    - android: `204 empty`
    - b33p: `204 empty`
    - desktop: `204 empty`
    - presage-cli: `-`
  - body:
    - android: `json pqLastResortPreKey:null pqPreKeys:array pqPreKeys[].keyId:number pqPreKeys[].publicKey:string pqPreKeys[].signature:string pqPreKeys[]:object preKeys:array preKeys[].keyId:number preKeys[].publicKey:string preKeys[]:object signedPreKey:null`
    - b33p: `json identityKey:string pqLastResortKey.keyId:number pqLastResortKey.publicKey:string pqLastResortKey.signature:string pqLastResortKey:object pqPreKeys:array pqPreKeys[].keyId:number pqPreKeys[].publicKey:string pqPreKeys[].signature:string pqPreKeys[]:object preKeys:array preKeys[].keyId:number preKeys[].publicKey:string preKeys[]:object signedPreKey.keyId:number signedPreKey.publicKey:string signedPreKey.signature:string signedPreKey:object`
    - desktop: `json pqPreKeys:array pqPreKeys[].keyId:number pqPreKeys[].publicKey:string pqPreKeys[].signature:string pqPreKeys[]:object preKeys:array preKeys[].keyId:number preKeys[].publicKey:string preKeys[]:object`
    - presage-cli: `json identityKey:string pqLastResortKey.keyId:number pqLastResortKey.publicKey:string pqLastResortKey.signature:string pqLastResortKey:object pqPreKeys:array pqPreKeys[].keyId:number pqPreKeys[].publicKey:string pqPreKeys[].signature:string pqPreKeys[]:object preKeys:array preKeys[].keyId:number preKeys[].publicKey:string preKeys[]:object signedPreKey.keyId:number signedPreKey.publicKey:string signedPreKey.signature:string signedPreKey:object`
  - content-type:
    - android: `application/json`
    - b33p: `application/json`
    - desktop: `application/json; charset=utf-8`
    - presage-cli: `application/json`
  - headers:
    - android: `content-type`
    - b33p: `content-type`
    - desktop: `authorization content-type x-signal-agent`
    - presage-cli: `content-type`
  - x-signal-agent:
    - android: (none)
    - b33p: (none)
    - desktop: `OWD`
    - presage-cli: (none)

## Attachments

Downloads (`GET /attachments/{id}`) and uploads (the requests carrying the bytes) per check; *again*: downloads of an attachment the client had downloaded before.

| check | client | downloads | bytes | again | uploads | bytes |
|---|---|---|---|---|---|---|
| b33p links to Android, with the message history | android | 0 | 0 B | 0 | 1 | 64 KiB |
| b33p links to Android, with the message history | b33p | 1 | 64 KiB | 0 | 0 | 0 B |
| Desktop shows its inbox | android | 0 | 0 B | 0 | 1 | 592 B |
| Desktop shows its inbox | b33p | 1 | 592 B | 0 | 0 | 0 B |
| text from the peer reaches b33p | presage-rs | 0 | 0 B | 0 | 1 | 592 B |
| text from the peer reaches Android | desktop | 1 | 592 B | 0 | 0 | 0 B |
| two pictures from Desktop reach b33p and Android, and download | android | 2 | 225 KiB | 0 | 0 | 0 B |
| two pictures from Desktop reach b33p and Android, and download | b33p | 2 | 225 KiB | 0 | 0 | 0 B |
| two pictures from Desktop reach b33p and Android, and download | desktop | 0 | 0 B | 0 | 2 | 225 KiB |
| a photo from b33p reaches Desktop, and Android as a sync | android | 1 | 8.1 MiB | 0 | 0 | 0 B |
| a photo from b33p reaches Desktop, and Android as a sync | b33p | 1 | 8.1 MiB | 0 | 1 | 8.1 MiB |
| a photo from b33p reaches Desktop, and Android as a sync | desktop | 1 | 8.1 MiB | 0 | 0 | 0 B |
| b33p restarted shows the chat's pictures again | b33p | 3 | 8.3 MiB | 3 | 0 | 0 B |
| b33p's viewer opens Desktop's photo | b33p | 1 | 152 KiB | 1 | 0 | 0 B |
| **all checks** | android | 3 | 8.3 MiB | 0 | 2 | 65 KiB |
| **all checks** | b33p | 9 | 16.8 MiB | 4 | 1 | 8.1 MiB |
| **all checks** | desktop | 2 | 8.1 MiB | 0 | 2 | 225 KiB |
| **all checks** | presage-rs | 0 | 0 B | 0 | 1 | 592 B |

## Check by check

### a presage-cli peer registers (passed, 1.3 s)

- presage-cli:
  - 12:00:03.023 `GET /v1/websocket/ 101`
  - 12:00:03.033 `PUT /v1/profile 200`
  - 12:00:03.041 `PUT /v1/accounts/attributes 204`
  - 12:00:03.119 `<= PUT /api/v1/queue/empty 200`
  - 12:00:03.124 `GET /v1/profile/{aci}/{id} 200`
  - 12:00:03.144 `GET /v2/keys?identity={v} 200`
  - 12:00:03.327 `GET /v1/websocket/ 101`
  - 12:00:03.330 `GET /v1/accounts/whoami 200`
- presage-rs:
  - 12:00:02.366 `GET /v1/websocket/ 101`
  - 12:00:02.418 `POST /v1/verification/session 200`
  - 12:00:02.583 `PATCH /v1/verification/session/{id} 200`
  - 12:00:02.616 `POST /v1/verification/session/{id}/code 200`
  - 12:00:02.634 `PUT /v1/verification/session/{id}/code 200`
  - 12:00:02.761 `POST /v1/registration 200`
  - 12:00:03.036 `GET /v1/websocket/ 101`
  - 12:00:03.334 `GET /v1/websocket/`

### Desktop links to the peer (passed, 0.7 s)

- desktop:
  - 12:00:04.810 `CONNECT /v1/websocket/provisioning/ 200`
  - 12:00:04.860 `<= PUT /v1/address 200`
  - 12:00:05.069 `<= PUT /v1/message`
- presage-cli:
  - 12:00:05.019 `GET /v1/websocket/ 101`
  - 12:00:05.027 `GET /v1/devices/provisioning/code 200`
  - 12:00:05.030 `PUT /v1/accounts/attributes 204`
  - 12:00:05.035 `<= PUT /api/v1/queue/empty 200`
  - 12:00:05.049 `PUT /v1/provisioning/{id} 204`
  - 12:00:05.050 `GET /v2/keys?identity={v} 200`
  - 12:00:05.064 `POST /v2/keys/check`
- presage-rs:
  - 12:00:05.027 `GET /v1/websocket/ 101`

### Android registers (passed, 14.5 s)

- android:
  - 12:00:31.254 `POST /v1/verification/session 200`
  - 12:00:31.591 `PATCH /v1/verification/session/{id} 200`
  - 12:00:31.625 `POST /v1/verification/session/{id}/code 200`
  - 12:00:32.336 `PUT /v1/verification/session/{id}/code 200`
  - 12:00:32.416 `POST /v1/registration 200`
  - 12:00:32.532 `CONNECT /v1/websocket/ 200` ×3
  - 12:00:32.580 `<= PUT /api/v1/queue/empty`
  - 12:00:32.622 `GET /v1/certificate/delivery 200`
  - 12:00:32.623 `GET /v2/keys?identity={v} 200`
  - 12:00:32.706 `GET /v1/certificate/delivery?includeE164={v} 200`
  - 12:00:32.732 `PUT /v1/accounts/attributes 204`
  - 12:00:32.874 `PUT /v2/keys?identity={v} 204`
  - 12:00:32.940 `GET /v2/keys?identity={v} 200`
  - 12:00:33.051 `PUT /v2/keys?identity={v} 204`
  - 12:00:34.609 `PUT /v1/accounts/attributes 204`
  - 12:00:34.640 `GET /v1/storage/auth 200`
  - 12:00:34.667 `GET /v1/storage/manifest 404`
  - 12:00:34.741 `GET /v1/storage/auth 200`
  - 12:00:34.748 `PUT /v1/storage?sendRemoteManifestOnConflict={v} 200`
  - 12:00:34.793 `CONNECT /v1/websocket/ 200`
  - 12:00:34.805 `<= PUT /api/v1/queue/empty`
  - 12:00:34.839 `PUT /v1/accounts/attributes 204`
  - 12:00:34.865 `GET /v1/storage/auth 200`
  - 12:00:34.878 `GET /v1/storage/manifest 200`
  - 12:00:34.930 `GET /v1/storage/auth 200`
  - 12:00:34.935 `PUT /v1/storage?sendRemoteManifestOnConflict={v} 200`
  - 12:00:34.960 `PUT /v1/accounts/attributes 204` ×2
  - 12:00:36.013 `PUT /v1/profile 200`
  - 12:00:36.017 `PUT /v1/archives/backupid 204`
  - 12:00:36.022 `GET /v1/storage/auth 200`
  - 12:00:36.058 `GET /v1/profile/{aci}/{id}/{id}?credentialType={v} 200`
  - 12:00:36.064 `GET /v1/storage/manifest/version/{n} 204`
  - 12:00:36.087 `GET /v1/storage/auth 200`
  - 12:00:36.137 `PUT /v1/storage?sendRemoteManifestOnConflict={v} 200`
  - 12:00:36.167 `GET /v2/directory/auth 200`
  - 12:00:36.204 `CONNECT /v1/{id}/discovery`
  - 12:00:36.218 `GET /v1/accounts/whoami 200`
  - 12:00:36.276 `GET /v1/storage/auth 200`
  - 12:00:36.283 `PUT /v1/storage?sendRemoteManifestOnConflict={v} 200`
  - 12:00:36.402 `GET /v1/keepalive 200`
  - 12:00:36.410 `CONNECT /v1/websocket/ 200`
  - 12:00:36.463 `GET /v1/keepalive 200`
- desktop:
  - 12:00:36.718 `GET /v1/keepalive 200`

### Android writes a note to itself (passed, 5.6 s)

- android:
  - 12:00:43.914 `GET /v2/directory/auth 200`
  - 12:00:43.930 `CONNECT /v1/{id}/discovery`
  - 12:00:44.855 `GET /v1/profile/{aci}/{id} 200`
  - 12:00:44.929 `GET /v1/accounts/whoami 200`

### b33p links to Android, with the message history (passed, 24.6 s)

- android:
  - 12:00:52.212 `POST /org.signal.chat.device.Devices/GetDevices 0`
  - 12:01:00.713 `GET /v1/devices/provisioning/code 200`
  - 12:01:00.725 `POST /org.signal.chat.device.Devices/GetDevices 0`
  - 12:01:00.752 `PUT /v1/provisioning/{id} 204`
  - 12:01:00.800 `GET /v1/devices/wait_for_linked_device/{id}?timeout={v} 200`
  - 12:01:04.841 `GET /v1/keepalive 200`
  - 12:01:10.915 `POST /org.signal.chat.attachments.Attachments/GetUploadForm 0`
  - 12:01:10.943 `POST /upload/attachments 201`
  - 12:01:11.025 `PUT /v1/devices/transfer_archive 200`
  - 12:01:11.172 `<= PUT /api/v1/message 200`
  - 12:01:11.219 `POST /org.signal.chat.attachments.Attachments/GetUploadForm 0`
  - 12:01:11.220 `GET /v2/keys?identity={v} 200`
  - 12:01:11.224 `<= PUT /api/v1/message 200` ×2
  - 12:01:11.238 `POST /upload/attachments 201`
- b33p:
  - 12:00:48.103 `GET /v1/websocket/provisioning/ 101`
  - 12:00:48.106 `<= PUT /v1/address 200`
  - 12:01:00.758 `<= PUT /v1/message 200`
  - 12:01:00.773 `PUT /v1/devices/link 200`
  - 12:01:01.131 `GET /v1/websocket/ 101`
  - 12:01:01.138 `GET /v1/storage/auth 200`
  - 12:01:01.148 `GET /v1/storage/manifest 200`
  - 12:01:01.157 `GET /v1/devices/transfer_archive?timeout={v} 200`
  - 12:01:11.048 `GET /attachments/{id} 200`
  - 12:01:11.094 `GET /v1/websocket/ 101`
  - 12:01:11.097 `GET /v1/certificate/delivery?includeE164={v} 200`
  - 12:01:11.105 `<= PUT /api/v1/queue/empty 200`
  - 12:01:11.108 `GET /v2/keys/{aci}/* 200`
  - 12:01:11.160 `PUT /v1/messages/{aci} 200` ×3
  - 12:01:11.236 `PUT /v1/devices/capabilities 204`
  - 12:01:11.238 `GET /v1/storage/auth 200`
- desktop:
  - 12:01:06.751 `GET /v1/keepalive 200`

### b33p knows Android's account (passed, 0.0 s)

- b33p:
  - 12:01:11.244 `GET /v1/storage/manifest/version/{n} 200`

### Desktop shows its inbox (passed, 55.6 s)

- android:
  - 12:01:11.252 `GET /v2/keys?identity={v} 200`
  - 12:01:11.259 `HEAD /upload/attachments/{id} 200`
  - 12:01:11.272 `PATCH /upload/attachments/{id} 204`
  - 12:01:11.287 `PUT /v1/messages/{aci}?story={v} 200` ×3
  - 12:01:34.843 `GET /v1/keepalive 200` ×2
- b33p:
  - 12:01:11.255 `PUT /v1/storage/read 200`
  - 12:01:11.256 `GET /v2/keys?identity={v} 200`
  - 12:01:11.273 `POST /v2/keys/check 200`
  - 12:01:11.295 `<= PUT /api/v1/message 200` ×2
  - 12:01:11.358 `PUT /v2/keys?identity={v} 204`
  - 12:01:11.397 `<= PUT /api/v1/message 200`
  - 12:01:11.399 `GET /v2/keys?identity={v} 200`
  - 12:01:11.408 `POST /v2/keys/check 200`
  - 12:01:11.409 `GET /attachments/{id} 200`
  - 12:01:11.480 `PUT /v2/keys?identity={v} 204`
  - 12:01:13.386 `GET /v1/storage/auth 200`
  - 12:01:13.392 `GET /v1/storage/manifest/version/{n} 204`
  - 12:01:31.135 `GET /v1/keepalive 200` ×3
- desktop:
  - 12:01:36.758 `GET /v1/keepalive 200`
  - 12:02:06.519 `GET /v1/certificate/auth/group?redemptionEndSeconds={v}&redemptionStartSeconds={v}&v101={v}&zkcCredential={v} 200`
  - 12:02:06.519 `PUT /v1/devices/capabilities 204`
  - 12:02:06.519 `POST /org.signal.chat.device.Devices/GetDevices 0`
  - 12:02:06.763 `GET /v1/keepalive 200`

### text from the peer reaches b33p (passed, 0.9 s)

- android:
  - 12:02:07.636 `<= PUT /api/v1/message 200`
- b33p:
  - 12:02:07.634 `<= PUT /api/v1/message 200`
  - 12:02:07.726 `GET /v1/profile/{aci}/{id} 200`
- desktop:
  - 12:02:07.104 `GET /stickers/{id}/manifest.proto 404` ×11
  - 12:02:07.373 `<= PUT /api/v1/message 200` ×4
- presage-cli:
  - 12:02:07.251 `GET /v1/websocket/ 101`
  - 12:02:07.264 `GET /v1/certificate/delivery?includeE164={v} 200`
  - 12:02:07.287 `PUT /v1/accounts/attributes 204`
  - 12:02:07.291 `GET /v2/keys/{aci}/* 200`
  - 12:02:07.299 `<= PUT /api/v1/message 200` ×6
  - 12:02:07.326 `GET /v4/attachments/form/upload?uploadLength={v} 200`
  - 12:02:07.336 `GET /v2/keys?identity={v} 200`
  - 12:02:07.340 `PUT /v1/messages/{aci} 200`
  - 12:02:07.357 `GET /v1/storage/auth 200`
  - 12:02:07.390 `POST /v2/keys/check 200`
  - 12:02:07.423 `GET /v1/storage/manifest 200`
  - 12:02:07.452 `PUT /v2/keys?identity={v}`
  - 12:02:07.473 `GET /v1/storage/auth 200`
  - 12:02:07.475 `<= PUT /api/v1/queue/empty 200`
  - 12:02:07.477 `PUT /v1/messages/{aci} 409`
  - 12:02:07.482 `PUT /v1/messages/{aci} 200`
  - 12:02:07.499 `GET /v1/storage/manifest 200`
  - 12:02:07.531 `GET /v2/keys/{aci}/{n} 200`
  - 12:02:07.590 `PUT /v1/messages/{aci} 200` ×2
  - 12:02:07.654 `GET /v1/profile/{aci}/{id}`
- presage-rs:
  - 12:02:07.274 `GET /v1/websocket/ 101`
  - 12:02:07.382 `POST /upload/attachments 201`
  - 12:02:07.415 `HEAD /upload/attachments/{id} 200`
  - 12:02:07.423 `PATCH /upload/attachments/{id} 204`

### text from the peer reaches Android (passed, 5.4 s)

- android:
  - 12:02:07.827 `GET /v1/profile/{aci}/{id}/{id}?credentialType={v} 200`
  - 12:02:07.848 `GET /v1/storage/auth 200`
  - 12:02:07.962 `PUT /v1/storage?sendRemoteManifestOnConflict={v} 200`
  - 12:02:08.005 `GET /v2/keys?identity={v} 200` ×2
  - 12:02:08.241 `PUT /v1/messages/{aci}?story={v} 200`
  - 12:02:08.287 `GET /v2/directory/auth 200`
  - 12:02:10.142 `CONNECT /v1/{id}/discovery`
  - 12:02:10.283 `GET /v1/storage/auth 200`
  - 12:02:10.340 `PUT /v1/storage?sendRemoteManifestOnConflict={v} 200`
  - 12:02:10.463 `PUT /v1/messages/{aci}?story={v} 200` ×2
- b33p:
  - 12:02:08.284 `<= PUT /api/v1/message 200`
  - 12:02:08.332 `GET /v1/storage/auth 200`
  - 12:02:08.400 `GET /v1/storage/manifest 200`
  - 12:02:10.469 `GET /v1/storage/auth 200`
  - 12:02:10.489 `<= PUT /api/v1/message 200`
  - 12:02:10.511 `GET /v1/storage/manifest/version/{n} 200`
  - 12:02:10.522 `GET /v1/storage/auth 200`
  - 12:02:10.563 `GET /v1/storage/manifest 200`
  - 12:02:10.564 `PUT /v1/storage/read 200`
  - 12:02:11.100 `GET /v1/keepalive 200`
  - 12:02:12.209 `<= PUT /api/v1/message 200`
  - 12:02:12.661 `GET /v1/storage/auth 200`
  - 12:02:12.688 `GET /v1/storage/manifest/version/{n} 204`
- desktop:
  - 12:02:08.012 `<= PUT /api/v1/message 200`
  - 12:02:08.124 `GET /attachments/{id} 200`
  - 12:02:08.709 `GET /v1/storage/auth 200`
  - 12:02:08.791 `GET /v1/storage/manifest/version/{n} 204`
  - 12:02:08.838 `PUT /v1/storage/ 200`
  - 12:02:08.926 `PUT /v1/messages/{aci}?story={v} 200`

### the peer's first message is a message request in b33p, accepted (passed, 0.1 s)

- b33p:
  - 12:02:13.298 `PUT /v1/messages/{aci} 409`

### no client lost a message (passed, 0.6 s)

- android:
  - 12:02:13.421 `<= PUT /api/v1/message 200`
  - 12:02:13.452 `GET /v1/storage/auth 200`
  - 12:02:13.460 `<= PUT /api/v1/message 200`
  - 12:02:13.461 `PUT /v1/storage?sendRemoteManifestOnConflict={v} 200`
  - 12:02:13.502 `PUT /v1/messages/{aci}?story={v} 200`
- b33p:
  - 12:02:13.305 `GET /v2/keys/{aci}/{n} 200`
  - 12:02:13.390 `PUT /v1/messages/{aci} 200` ×2
  - 12:02:13.430 `GET /v1/profile/{aci}/{id} 200`
  - 12:02:13.452 `PUT /v1/messages/{aci} 200`
  - 12:02:13.508 `<= PUT /api/v1/message 200`
  - 12:02:13.527 `GET /v1/storage/auth 200`
  - 12:02:13.532 `GET /v1/storage/manifest 200`
- desktop:
  - 12:02:13.399 `<= PUT /api/v1/message 200`
  - 12:02:13.801 `CONNECT /v1/websocket/ 200`
  - 12:02:13.807 `GET /v1/profile/{aci}/{id}/{id}?credentialType={v} 200`

### Desktop has the peer's conversation with Android (passed, 0.1 s)

- desktop:
  - 12:02:14.313 `GET /v2/config 304`
  - 12:02:14.357 `GET /v1/profile/{aci}/{id} 200`

### two pictures from Desktop reach b33p and Android, and download (passed, 7.6 s)

- android:
  - 12:02:15.777 `<= PUT /api/v1/message 200`
  - 12:02:15.832 `PUT /v1/messages/{aci}?story={v} 200`
  - 12:02:15.840 `GET /attachments/{id} 200` ×2
  - 12:02:15.950 `PUT /v1/messages/{aci}?story={v} 200` ×2
- b33p:
  - 12:02:15.545 `GET /v1/storage/auth 200`
  - 12:02:15.552 `GET /v1/storage/manifest/version/{n} 200`
  - 12:02:15.561 `PUT /v1/storage/read 200`
  - 12:02:15.777 `<= PUT /api/v1/message 200`
  - 12:02:15.827 `GET /attachments/{id} 200` ×2
  - 12:02:15.847 `PUT /v1/messages/{aci} 200`
  - 12:02:15.957 `<= PUT /api/v1/message 200`
- desktop:
  - 12:02:15.147 `GET /v4/attachments/form/upload?uploadLength={v} 200` ×2
  - 12:02:15.658 `POST /upload/attachments 201` ×2
  - 12:02:15.727 `GET /v1/certificate/delivery?includeE164={v} 200`
  - 12:02:15.741 `PUT /v1/messages/{aci}?story={v} 409`
  - 12:02:15.748 `GET /v2/keys/{aci}/* 200`
  - 12:02:15.770 `PUT /v1/messages/{aci}?story={v} 200` ×2
  - 12:02:15.840 `<= PUT /api/v1/message 200` ×3

### a photo from b33p reaches Desktop, and Android as a sync (passed, 12.5 s)

- android:
  - 12:02:27.861 `<= PUT /api/v1/message 200`
  - 12:02:27.901 `GET /attachments/{id} 200`
  - 12:02:28.454 `<= PUT /api/v1/message 200`
- b33p:
  - 12:02:22.079 `PUT /v1/messages/{aci} 200`
  - 12:02:27.699 `GET /v4/attachments/form/upload?uploadLength={v} 200`
  - 12:02:27.707 `POST /upload/attachments 201`
  - 12:02:27.719 `HEAD /upload/attachments/{id} 200`
  - 12:02:27.729 `PATCH /upload/attachments/{id} 204`
  - 12:02:27.844 `PUT /v1/messages/{aci} 200` ×2
  - 12:02:27.877 `GET /attachments/{id} 200`
  - 12:02:28.453 `<= PUT /api/v1/message 200`
  - 12:02:31.134 `GET /v1/keepalive 200`
- desktop:
  - 12:02:22.085 `<= PUT /api/v1/message 200` ×2
  - 12:02:28.446 `PUT /v1/messages/{aci}?story={v} 200`
  - 12:02:28.677 `GET /attachments/{id} 200`

### b33p restarted shows the chat's pictures again (passed, 8.2 s)

- android:
  - 12:02:34.843 `GET /v1/keepalive 200`
- b33p:
  - 12:02:36.778 `GET /v1/websocket/ 101`
  - 12:02:36.778 `GET /v1/storage/auth 200`
  - 12:02:36.782 `GET /v1/profile/{aci}/{id} 200`
  - 12:02:36.784 `GET /v1/websocket/ 101`
  - 12:02:36.786 `<= PUT /api/v1/queue/empty 200`
  - 12:02:36.788 `GET /v1/storage/manifest/version/{n} 204`
  - 12:02:36.790 `PUT /v1/devices/capabilities 204`
  - 12:02:36.796 `GET /v1/profile/{aci}/{id} 200`
  - 12:02:36.800 `GET /attachments/{id} 200`
  - 12:02:36.801 `GET /v2/keys?identity={v} 200`
  - 12:02:36.812 `GET /attachments/{id} 200` ×2
  - 12:02:36.818 `POST /v2/keys/check 200`
  - 12:02:37.080 `GET /v2/keys?identity={v} 200`
  - 12:02:37.392 `POST /v2/keys/check 200`
- desktop:
  - 12:02:36.780 `GET /v1/keepalive 200`

### b33p's viewer opens Desktop's photo (passed, 6.5 s)

- b33p:
  - 12:02:42.717 `GET /attachments/{id} 200`
- desktop:
  - 12:02:43.812 `GET /v1/keepalive 200`

## Contents

The fields of the contents b33p took in, by who sent them (b33p's `test-servers` content log). **Bold**: only one sender filled it in.

### dataMessage (desktop 1, presage-cli 1)

| field | android | desktop | presage-cli |
|---|---|---|---|
| **`dataMessage.attachments.10:varint`** |  | ✓ |  |
| **`dataMessage.attachments.12:string`** |  | ✓ |  |
| **`dataMessage.attachments.13:varint`** |  | ✓ |  |
| **`dataMessage.attachments.14:varint`** |  | ✓ |  |
| **`dataMessage.attachments.15:string`** |  | ✓ |  |
| **`dataMessage.attachments.20:bytes`** |  | ✓ |  |
| **`dataMessage.attachments.2:string`** |  | ✓ |  |
| **`dataMessage.attachments.3:bytes`** |  | ✓ |  |
| **`dataMessage.attachments.4:varint`** |  | ✓ |  |
| **`dataMessage.attachments.6:bytes`** |  | ✓ |  |
| **`dataMessage.attachments.9:varint`** |  | ✓ |  |
| **`dataMessage.attachments:message`** |  | ✓ |  |
| `dataMessage.body:string` |  | ✓ | ✓ |
| `dataMessage.expireTimerVersion:varint` |  | ✓ | ✓ |
| **`dataMessage.flags:varint`** |  | ✓ |  |
| **`dataMessage.isViewOnce:varint`** |  | ✓ |  |
| `dataMessage.profileKey:bytes` |  | ✓ | ✓ |
| `dataMessage.requiredProtocolVersion:varint` |  | ✓ | ✓ |
| `dataMessage.timestamp:varint` |  | ✓ | ✓ |
| `dataMessage:message` |  | ✓ | ✓ |

### receiptMessage (desktop 1)

| field | android | desktop | presage-cli |
|---|---|---|---|
| `receiptMessage.timestamp:varint` |  | ✓ |  |
| `receiptMessage.type:varint` |  | ✓ |  |
| `receiptMessage:message` |  | ✓ |  |

### syncMessage.blocked (android 1)

| field | android | desktop | presage-cli |
|---|---|---|---|
| `syncMessage.blocked:bytes` | ✓ |  |  |
| `syncMessage.padding:bytes` | ✓ |  |  |
| `syncMessage:message` | ✓ |  |  |

### syncMessage.fetchLatest (android 3)

| field | android | desktop | presage-cli |
|---|---|---|---|
| `syncMessage.fetchLatest.1:varint` | ✓ |  |  |
| `syncMessage.fetchLatest:message` | ✓ |  |  |
| `syncMessage.padding:bytes` | ✓ |  |  |
| `syncMessage:message` | ✓ |  |  |

### syncMessage.keys (android 1)

| field | android | desktop | presage-cli |
|---|---|---|---|
| `syncMessage.keys.3:string` | ✓ |  |  |
| `syncMessage.keys.4:bytes` | ✓ |  |  |
| `syncMessage.keys:message` | ✓ |  |  |
| `syncMessage.padding:bytes` | ✓ |  |  |
| `syncMessage:message` | ✓ |  |  |

### syncMessage.read (android 2)

| field | android | desktop | presage-cli |
|---|---|---|---|
| `syncMessage.padding:bytes` | ✓ |  |  |
| `syncMessage.read.2:varint` | ✓ |  |  |
| `syncMessage.read.4:bytes` | ✓ |  |  |
| `syncMessage.read:message` | ✓ |  |  |
| `syncMessage:message` | ✓ |  |  |
