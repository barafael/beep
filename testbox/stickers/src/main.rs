//! Seeds a sticker pack in the box and installs it on a presage-cli account (TESTPLAN.md §2.11):
//! `testbox-stickers <sqlite db>`, in the presage container, as the box's servers are on its
//! clients' localhost only.
//!
//! As Signal's sticker creator does, with the account's own credentials: asks the server for
//! upload forms (`GET /v1/sticker/pack/form/{count}`, StickerController.java), encrypts the
//! manifest and each sticker with a key of its own (libsignal-service `sticker_cipher`), and
//! posts them to CDN0 at the keys the forms name. Then installs the pack the way a phone does
//! (presage `install_sticker_pack`): it downloads the pack back from CDN0 and tells the account's
//! other devices, b33p among them, with a sync message. Prints the pack's id, in hex.
//!
//! The account must have taken in what waits for it first (`presage-cli sync`, as the checks run
//! it): until then it knows no session with b33p's device, and the sync reaches nobody ("no
//! messages were encrypted"); in another process, so that its answers to b33p's requests go out
//! before the install's sync, not alongside it with the same session counter.

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

use std::io::Cursor;

use presage::Manager;
use presage::libsignal_service::configuration::Endpoint;
use presage::libsignal_service::prelude::ProtobufMessage;
use presage::libsignal_service::proto::{Pack, pack};
use presage::libsignal_service::push_service::{
    AttachmentV2UploadAttributes, HttpAuthOverride, ServiceError,
};
use presage::libsignal_service::sticker_cipher::{
    AttachmentCipherError, derive_key, encrypt_in_place,
};
use presage::model::identity::OnNewIdentity;
use presage_store_sqlite::{SqliteStore, SqliteStoreError};
use serde::Deserialize;
use testbox_tools::UsageError;

const TITLE: &str = "testbox stickers";
const AUTHOR: &str = "b33p testbox";
/// Each sticker's emoji and colour: two, so that a pack of more than one is checked.
/// As many as the checks expect (`testbox_tools::STICKERS`).
const STICKERS: [(&str, [u8; 3]); testbox_tools::STICKERS as usize] =
    [("🟥", [0xd0, 0x30, 0x30]), ("🟦", [0x30, 0x60, 0xd0])];
/// Signal's stickers are 512 px square; small ones decode the same.
const EDGE: u32 = 64;
const USAGE: &str = "testbox-stickers <sqlite db>";

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let outcome: Result<(), Box<dyn std::error::Error + Send + Sync>> =
        match testbox_tools::argument(USAGE) {
            // presage spawns its websockets' tasks onto the thread's `LocalSet`.
            Ok(Some(db)) => tokio::task::LocalSet::new()
                .run_until(seed(&db))
                .await
                .map_err(Into::into),
            Ok(None) => Err(UsageError {
                usage: USAGE.to_owned(),
            }
            .into()),
            Err(error) => Err(error.into()),
        };
    testbox_tools::exit("testbox-stickers", outcome)
}

/// What keeps the pack from being seeded or installed.
#[derive(Debug, thiserror::Error)]
enum SeedError {
    #[error("opening presage's store {db}")]
    OpenStore {
        db: String,
        #[source]
        source: SqliteStoreError,
    },
    /// presage's errors are boxed here and in `Install`: they are big, and the rest small.
    #[error("loading the registered account")]
    LoadAccount {
        #[source]
        source: Box<presage::Error<SqliteStoreError>>,
    },
    #[error("asking for the sticker forms")]
    FormsRequest {
        #[source]
        source: ServiceError,
    },
    #[error("sending the request for the sticker forms")]
    FormsSend {
        #[source]
        source: reqwest::Error,
    },
    #[error("reading the sticker forms")]
    FormsBody {
        #[source]
        source: reqwest::Error,
    },
    #[error("the sticker forms refused: {status}: {body}")]
    FormsRefused {
        status: reqwest::StatusCode,
        body: String,
    },
    #[error("reading the sticker forms {body}")]
    ParseForms {
        body: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("deriving the pack's key")]
    PackKey {
        #[source]
        source: AttachmentCipherError,
    },
    #[error("uploading the manifest")]
    UploadManifest {
        #[source]
        source: ServiceError,
    },
    #[error("sticker {id}'s index")]
    StickerIndex {
        id: u32,
        #[source]
        source: std::num::TryFromIntError,
    },
    /// The server handed out a form for a sticker the pack doesn't have.
    #[error("a form for sticker {id}, of {count} stickers")]
    UnknownSticker { id: u32, count: usize },
    #[error("drawing sticker {id}'s picture")]
    Picture {
        id: u32,
        #[source]
        source: png::EncodingError,
    },
    #[error("uploading sticker {id}")]
    UploadSticker {
        id: u32,
        #[source]
        source: ServiceError,
    },
    #[error("reading the pack's id {pack_id:?}")]
    PackId {
        pack_id: String,
        #[source]
        source: hex::FromHexError,
    },
    #[error("installing the pack")]
    Install {
        #[source]
        source: Box<presage::Error<SqliteStoreError>>,
    },
}

/// The upload forms the server hands out: one for the manifest, one per sticker, and the id it
/// chose for the pack (StickerPackFormUploadAttributes.java).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StickerForms {
    pack_id: String,
    manifest: AttachmentV2UploadAttributes,
    stickers: Vec<StickerForm>,
}

#[derive(Deserialize)]
struct StickerForm {
    id: u32,
    #[serde(flatten)]
    form: AttachmentV2UploadAttributes,
}

async fn seed(db: &str) -> Result<(), SeedError> {
    let store = SqliteStore::open(db, OnNewIdentity::Trust)
        .await
        .map_err(|source| SeedError::OpenStore {
            db: db.to_owned(),
            source,
        })?;
    let mut manager =
        Manager::load_registered(store)
            .await
            .map_err(|source| SeedError::LoadAccount {
                source: Box::new(source),
            })?;
    let push = manager.identified_push_service();

    let response = push
        .request(
            reqwest::Method::GET,
            Endpoint::service(format!("/v1/sticker/pack/form/{}", STICKERS.len())),
            HttpAuthOverride::NoOverride,
        )
        .map_err(|source| SeedError::FormsRequest { source })?
        .send()
        .await
        .map_err(|source| SeedError::FormsSend { source })?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|source| SeedError::FormsBody { source })?;
    if !status.is_success() {
        return Err(SeedError::FormsRefused { status, body });
    }
    let forms: StickerForms = match serde_json::from_str(&body) {
        Ok(forms) => forms,
        Err(source) => return Err(SeedError::ParseForms { body, source }),
    };

    let pack_key: [u8; 32] = rand::random();
    let key = derive_key(&pack_key).map_err(|source| SeedError::PackKey { source })?;
    let encrypted = |mut plaintext: Vec<u8>| {
        encrypt_in_place(rand::random(), key, &mut plaintext);
        plaintext
    };

    let stickers: Vec<pack::Sticker> = STICKERS
        .iter()
        .zip(0u32..)
        .map(|((emoji, _), id)| pack::Sticker {
            id: Some(id),
            emoji: Some((*emoji).to_owned()),
            content_type: Some("image/png".to_owned()),
        })
        .collect();
    let manifest = Pack {
        title: Some(TITLE.to_owned()),
        author: Some(AUTHOR.to_owned()),
        cover: stickers.first().cloned(),
        stickers,
    };
    let mut cdn0 = push;
    cdn0.upload_to_cdn0(
        "",
        forms.manifest,
        "manifest".to_owned(),
        Cursor::new(encrypted(manifest.encode_to_vec())),
    )
    .await
    .map_err(|source| SeedError::UploadManifest { source })?;
    for StickerForm { id, form } in forms.stickers {
        let index = usize::try_from(id).map_err(|source| SeedError::StickerIndex { id, source })?;
        let (_, colour) = STICKERS.get(index).ok_or(SeedError::UnknownSticker {
            id,
            count: STICKERS.len(),
        })?;
        let picture = picture(*colour).map_err(|source| SeedError::Picture { id, source })?;
        cdn0.upload_to_cdn0("", form, format!("{id}"), Cursor::new(encrypted(picture)))
            .await
            .map_err(|source| SeedError::UploadSticker { id, source })?;
    }

    let pack_id = hex::decode(&forms.pack_id).map_err(|source| SeedError::PackId {
        pack_id: forms.pack_id.clone(),
        source,
    })?;
    manager
        .install_sticker_pack(&pack_id, &pack_key)
        .await
        .map_err(|source| SeedError::Install {
            source: Box::new(source),
        })?;
    println!("{}", forms.pack_id);
    Ok(())
}

/// A square of `colour`, as a PNG.
fn picture(colour: [u8; 3]) -> Result<Vec<u8>, png::EncodingError> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, EDGE, EDGE);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    let pixels: Vec<u8> = (0..EDGE * EDGE).flat_map(|_| colour).collect();
    writer.write_image_data(&pixels)?;
    writer.finish()?;
    Ok(bytes)
}
