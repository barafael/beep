//! CDN0 for the box: a single-node Garage with the bucket `cdn`, and in front
//! of it, on the clients' localhost, TLS with the keys' certificate.
//!
//! Clients post S3 POST-policy forms to the CDN's root, the key in the form (Signal-Desktop
//! WebAPI.preload.ts `${cdnUrlObject['0']}/`; libsignal-service-rs `upload_to_cdn0("")`), and read
//! objects at their keys. Garage takes the bucket from the path or the host name, so uploads go
//! to `/cdn` on its S3 API, and reads to its website endpoint, which serves without credentials.
//! Bodies stream through both ways.

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

use std::net::SocketAddr;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use serde::Serialize;
use testbox_tools::keys::{self, Cdn0Credentials, KeyFile};
use testbox_tools::process::{output_async, spawn_logged, supervise, wait_for};
use testbox_tools::{Context, Describe, Error, Result};
use tokio::process::{Child, Command};

const CONFIG: &str = "/etc/garage.toml";
/// Where Garage listens, on the container's localhost.
struct Garage {
    s3_port: u16,
    rpc_address: &'static str,
    web_port: u16,
    /// Its website endpoint serves a bucket at `<bucket>.<web_domain>`.
    web_domain: &'static str,
}

const GARAGE: Garage = Garage {
    s3_port: 3900,
    rpc_address: "127.0.0.1:3901",
    web_port: 3902,
    web_domain: "web.garage",
};
/// What Garage logs once each of its servers listens.
const GARAGE_READY: &[&str] = &[
    "S3 API server listening",
    "Web server listening",
    "Listening on 127.0.0.1:3901",
];
const GARAGE_PATIENCE: Duration = Duration::from_secs(60);
/// Headers of one connection, not of the request or answer passed on.
const HOP_BY_HOP: [HeaderName; 2] = [header::CONNECTION, header::TRANSFER_ENCODING];

/// Garage's configuration: one node, one replica, local only.
#[derive(Serialize)]
struct GarageConfig {
    metadata_dir: &'static str,
    data_dir: &'static str,
    db_engine: &'static str,
    replication_factor: u8,
    rpc_bind_addr: &'static str,
    rpc_public_addr: &'static str,
    /// Guards nothing reachable from outside the namespace, so a fixed one does.
    rpc_secret: String,
    s3_api: S3Api,
    s3_web: S3Web,
}

#[derive(Serialize)]
struct S3Api {
    api_bind_addr: String,
    s3_region: &'static str,
}

#[derive(Serialize)]
struct S3Web {
    bind_addr: String,
    root_domain: String,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    testbox_tools::exit("cdn0", run().await)
}

async fn run() -> Result<()> {
    let credentials_path = KeyFile::Cdn0.path();
    let credentials: Cdn0Credentials = toml::from_str(&testbox_tools::read(&credentials_path)?)
        .describe(credentials_path.display().to_string())?;
    write_config().await?;
    let mut garage = start_garage().await?;
    set_up(&credentials).await?;
    // Until either fails, or SIGTERM stops Garage: as a container's first process, nothing else
    // ends the front.
    tokio::select! {
        served = serve() => served,
        supervised = supervise(&mut garage, "garage") => supervised,
    }
}

async fn write_config() -> Result<()> {
    let config = GarageConfig {
        metadata_dir: "/data/meta",
        data_dir: "/data/data",
        db_engine: "sqlite",
        replication_factor: 1,
        rpc_bind_addr: GARAGE.rpc_address,
        rpc_public_addr: GARAGE.rpc_address,
        rpc_secret: "0".repeat(64),
        s3_api: S3Api {
            api_bind_addr: format!("127.0.0.1:{}", GARAGE.s3_port),
            // Signal-Server signs its POST policies for test.yml's `cdn.region`.
            s3_region: keys::CDN0_BUCKET.region,
        },
        s3_web: S3Web {
            bind_addr: format!("127.0.0.1:{}", GARAGE.web_port),
            root_domain: format!(".{}", GARAGE.web_domain),
        },
    };
    let text = toml::to_string(&config).describe("garage's configuration")?;
    tokio::fs::write(CONFIG, text).await.context(CONFIG)
}

/// Starts Garage and returns once it says all its servers listen; its log goes on into ours.
async fn start_garage() -> Result<Child> {
    let (garage, heard) = spawn_logged(Command::new("garage").args(["-c", CONFIG, "server"]))?;
    wait_for(heard, GARAGE_READY, GARAGE_PATIENCE).await?;
    Ok(garage)
}

async fn garage(args: &[&str]) -> Result<String> {
    output_async(Command::new("garage").args(["-c", CONFIG]).args(args)).await
}

/// Whether a listing shows `word` as a word of its own.
fn lists(listing: &str, word: &str) -> bool {
    listing.split_whitespace().any(|listed| listed == word)
}

/// Layout, key and bucket, each only if missing: /data outlives the container.
async fn set_up(credentials: &Cdn0Credentials) -> Result<()> {
    let (node, status) = tokio::try_join!(garage(&["node", "id", "-q"]), garage(&["status"]))?;
    if status.contains("NO ROLE ASSIGNED") {
        // `id@address`; the layout takes the id.
        let node = node.trim();
        let node = node.split_once('@').map_or(node, |(id, _address)| id);
        garage(&["layout", "assign", "-z", "box", "-c", "1G", node]).await?;
        garage(&["layout", "apply", "--version", "1"]).await?;
    }
    // Only once the layout is applied: before, Garage answers listings "Layout not ready".
    let (keys, buckets) = tokio::try_join!(garage(&["key", "list"]), garage(&["bucket", "list"]))?;
    if !lists(&keys, &credentials.access_key) {
        garage(&[
            "key",
            "import",
            "--yes",
            "-n",
            "signal-server",
            &credentials.access_key,
            &credentials.secret_key,
        ])
        .await?;
    }
    if !lists(&buckets, keys::CDN0_BUCKET.name) {
        garage(&["bucket", "create", keys::CDN0_BUCKET.name]).await?;
        garage(&[
            "bucket",
            "allow",
            "--read",
            "--write",
            "--owner",
            keys::CDN0_BUCKET.name,
            "--key",
            &credentials.access_key,
        ])
        .await?;
        garage(&["bucket", "website", "--allow", keys::CDN0_BUCKET.name]).await?;
    }
    Ok(())
}

async fn serve() -> Result<()> {
    // The one provider built in (Cargo.toml).
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_installed| Error::Failed("a TLS provider is installed already".to_owned()))?;
    let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(
        KeyFile::TlsCertificate.path(),
        KeyFile::TlsKey.path(),
    )
    .await
    .context("the keys' certificate")?;
    let client = reqwest::Client::new();
    let app = axum::Router::new().fallback(move |request: Request| {
        let client = client.clone();
        async move { forward(&client, request).await }
    });
    eprintln!("CDN0 on {}", keys::localhost(keys::CDN0_PORT));
    axum_server::bind_rustls(SocketAddr::from(([127, 0, 0, 1], keys::CDN0_PORT)), tls)
        .serve(app.into_make_service())
        .await
        .context("serving CDN0")
}

/// One request to Garage, its answer back; what goes wrong on the way answers 502. Each is
/// logged as `cdn0: <method> <path> <status>`, which the checks wait on (a sticker downloaded).
async fn forward(client: &reqwest::Client, request: Request) -> Response {
    let method = request.method().to_string();
    let path = request.uri().path().to_owned();
    let response = match forward_to_garage(client, request).await {
        Ok(response) => response,
        Err(error) => {
            eprintln!("cdn0: {method} {path}: {error}");
            (StatusCode::BAD_GATEWAY, error.to_string()).into_response()
        }
    };
    eprintln!(
        "{}",
        testbox_tools::cdn0_request_line(&method, &path, response.status().as_u16())
    );
    response
}

async fn forward_to_garage(client: &reqwest::Client, request: Request) -> Result<Response> {
    let (mut parts, body) = request.into_parts();
    let (url, host, body) = match parts.method {
        Method::POST if parts.uri.path() == "/" => {
            let prefix = bucket_field(&parts.headers)?;
            // The form grows by the field.
            if let Some(length) = parts.headers.get(header::CONTENT_LENGTH) {
                let length: usize = length
                    .to_str()
                    .describe("the upload's length")?
                    .parse()
                    .describe("the upload's length")?;
                let length = length
                    .checked_add(prefix.len())
                    .ok_or("an upload past usize")?;
                parts
                    .headers
                    .insert(header::CONTENT_LENGTH, HeaderValue::from(length));
            }
            let form =
                futures_util::stream::once(async { Ok(prefix) }).chain(body.into_data_stream());
            (
                format!(
                    "http://127.0.0.1:{}/{}",
                    GARAGE.s3_port,
                    keys::CDN0_BUCKET.name
                ),
                format!("127.0.0.1:{}", GARAGE.s3_port),
                reqwest::Body::wrap_stream(form),
            )
        }
        Method::GET | Method::HEAD => (
            format!(
                "http://127.0.0.1:{}{}",
                GARAGE.web_port,
                parts.uri.path_and_query().map_or("/", |path| path.as_str())
            ),
            format!("{}.{}", keys::CDN0_BUCKET.name, GARAGE.web_domain),
            reqwest::Body::from(Vec::new()),
        ),
        method => {
            return Ok((
                StatusCode::METHOD_NOT_ALLOWED,
                format!("{method} {}", parts.uri),
            )
                .into_response());
        }
    };
    let mut headers = parts.headers;
    without_hops(&mut headers);
    headers.insert(
        header::HOST,
        HeaderValue::from_str(&host).describe("the upstream host")?,
    );
    let answer = client
        .request(parts.method.clone(), &url)
        .headers(headers)
        .body(body)
        .send()
        .await
        .describe(format!("{} {url}", parts.method))?;
    let status = answer.status();
    let mut headers = answer.headers().clone();
    without_hops(&mut headers);
    let mut response = Body::from_stream(answer.bytes_stream()).into_response();
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    Ok(response)
}

fn without_hops(headers: &mut HeaderMap) {
    for hop in HOP_BY_HOP {
        headers.remove(hop);
    }
}

/// A `bucket` field, to go first in the form. Garage checks the policy's `bucket` condition
/// against a form field ("Key 'bucket' is required in policy, but no value was provided"), where
/// S3 takes the bucket from the URL and clients send none. The signature covers the policy only,
/// so the added field leaves it valid.
fn bucket_field(headers: &HeaderMap) -> Result<Bytes> {
    let content_type = match headers.get(header::CONTENT_TYPE) {
        Some(value) => value.to_str().describe("an upload's content type")?,
        None => "",
    };
    let boundary = content_type
        .split(';')
        .find_map(|parameter| parameter.trim().strip_prefix("boundary="))
        .map(|boundary| boundary.trim_matches('"'))
        .ok_or_else(|| Error::Failed(format!("an upload that isn't a form: {content_type:?}")))?;
    Ok(Bytes::from(format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"bucket\"\r\n\r\n{}\r\n",
        keys::CDN0_BUCKET.name
    )))
}
