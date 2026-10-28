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
use presage::libsignal_service::push_service::{AttachmentV2UploadAttributes, HttpAuthOverride};
use presage::libsignal_service::sticker_cipher::{derive_key, encrypt_in_place};
use presage::model::identity::OnNewIdentity;
use presage_store_sqlite::SqliteStore;
use serde::Deserialize;
use testbox_tools::{Describe, Error, Result, WithContext};

const TITLE: &str = "testbox stickers";
const AUTHOR: &str = "b33p testbox";
/// Each sticker's emoji and colour: two, so that a pack of more than one is checked.
/// As many as the checks expect (`testbox_tools::STICKERS`).
const STICKERS: [(&str, [u8; 3]); testbox_tools::STICKERS as usize] =
    [("🟥", [0xd0, 0x30, 0x30]), ("🟦", [0x30, 0x60, 0xd0])];
/// Signal's stickers are 512 px square; small ones decode the same.
const EDGE: u32 = 64;

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let outcome = match testbox_tools::argument("testbox-stickers <sqlite db>") {
        // presage spawns its websockets' tasks onto the thread's `LocalSet`.
        Ok(Some(db)) => tokio::task::LocalSet::new().run_until(seed(&db)).await,
        Ok(None) => Err(Error::Failed(
            "usage: testbox-stickers <sqlite db>".to_owned(),
        )),
        Err(error) => Err(error),
    };
    testbox_tools::exit("testbox-stickers", outcome)
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

async fn seed(db: &str) -> Result<()> {
    let store = SqliteStore::open(db, OnNewIdentity::Trust)
        .await
        .describe(format!("presage's store {db}"))?;
    let mut manager = Manager::load_registered(store)
        .await
        .describe("the registered account")?;
    let push = manager.identified_push_service();

    let response = push
        .request(
            reqwest::Method::GET,
            Endpoint::service(format!("/v1/sticker/pack/form/{}", STICKERS.len())),
            HttpAuthOverride::NoOverride,
        )
        .describe("the sticker forms")?
        .send()
        .await
        .describe("the sticker forms")?;
    let status = response.status();
    let text = response.text().await.describe("the sticker forms")?;
    if !status.is_success() {
        return Err(Error::Failed(format!(
            "the sticker forms: {status}: {text}"
        )));
    }
    let forms: StickerForms =
        serde_json::from_str(&text).with_context(|| format!("the sticker forms: {text}"))?;

    let pack_key: [u8; 32] = rand::random();
    let key = derive_key(&pack_key).describe("the pack's key")?;
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
    .describe("uploading the manifest")?;
    for StickerForm { id, form } in forms.stickers {
        let colour = STICKERS
            .get(usize::try_from(id).describe("a sticker's id")?)
            .map(|(_, colour)| *colour)
            .ok_or_else(|| Error::Failed(format!("a form for sticker {id}, which there isn't")))?;
        cdn0.upload_to_cdn0(
            "",
            form,
            format!("{id}"),
            Cursor::new(encrypted(picture(colour)?)),
        )
        .await
        .describe(format!("uploading sticker {id}"))?;
    }

    let pack_id = hex::decode(&forms.pack_id).describe("the pack's id")?;
    manager
        .install_sticker_pack(&pack_id, &pack_key)
        .await
        .describe("installing the pack")?;
    println!("{}", forms.pack_id);
    Ok(())
}

/// A square of `colour`, as a PNG.
fn picture(colour: [u8; 3]) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, EDGE, EDGE);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().describe("a sticker's picture")?;
    let pixels: Vec<u8> = (0..EDGE * EDGE).flat_map(|_| colour).collect();
    writer
        .write_image_data(&pixels)
        .describe("a sticker's picture")?;
    writer.finish().describe("a sticker's picture")?;
    Ok(bytes)
}
