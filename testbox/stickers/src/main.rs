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
use presage::libsignal_service::configuration::{Endpoint, ServiceCredentials};
use presage::libsignal_service::prelude::ProtobufMessage;
use presage::libsignal_service::proto::{Pack, pack};
use presage::libsignal_service::protocol::DeviceId;
use presage::libsignal_service::push_service::{
    AttachmentV2UploadAttributes, HttpAuthOverride, PushService,
};
use presage::libsignal_service::sticker_cipher::{derive_key, encrypt_in_place};
use presage::libsignal_service::utils::TryIntoE164;
use presage::manager::RegistrationData;
use presage::model::identity::OnNewIdentity;
use presage_store_sqlite::SqliteStore;
use serde::Deserialize;
use testbox_tools::{Context, Describe, Error, Result, WithContext};

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

/// The account's password, which presage keeps to itself (`RegistrationData::password` is
/// `pub(crate)`), from the form presage stores it in: its serialized registration data. Not
/// needed once presage makes `identified_push_service` public (barafael/presage
/// `registered.rs:124`).
#[derive(Deserialize)]
struct Password {
    password: String,
}

async fn seed(db: &str) -> Result<()> {
    let store = SqliteStore::open(db, OnNewIdentity::Trust)
        .await
        .describe(format!("presage's store {db}"))?;
    let mut manager = Manager::load_registered(store)
        .await
        .describe("the registered account")?;
    let push = push_service(manager.registration_data())?;

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

/// A push service with the account's credentials, as presage builds one
/// (`Registered::credentials`).
fn push_service(data: &RegistrationData) -> Result<PushService> {
    let Password { password } =
        serde_json::from_str(&serde_json::to_string(data).context("the registration data")?)
            .context("the registration data")?;
    let credentials = ServiceCredentials {
        aci: Some(data.service_ids.aci),
        pni: Some(data.service_ids.pni),
        phonenumber: (&data.phone_number)
            .try_into_e164()
            .describe("the account's number")?,
        password: Some(password),
        device_id: data
            .device_id
            .map(DeviceId::try_from)
            .transpose()
            .describe("the account's device id")?,
    };
    Ok(PushService::new(
        data.signal_servers.clone(),
        Some(credentials),
        "testbox-stickers",
    ))
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
