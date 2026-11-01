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
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use serde::Serialize;
use testbox_tools::keys::{self, Cdn0Credentials, KeyFile};
use testbox_tools::process::{CommandError, output_async, spawn_logged, supervise, wait_for};
use testbox_tools::tls;
use tokio::process::Command;

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

/// What keeps CDN0 from starting, or from serving on.
#[derive(Debug, thiserror::Error)]
enum Cdn0Error {
    #[error("parsing the CDN's credentials {}", path.display())]
    Credentials {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("serializing garage's configuration")]
    SerializeConfig(#[source] toml::ser::Error),
    #[error("writing garage's configuration {}", path.display())]
    WriteConfig {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("waiting for garage's servers to listen")]
    GarageListening(#[source] testbox_tools::process::LogError),
    #[error("serving CDN0 on {address}")]
    Serve {
        address: SocketAddr,
        #[source]
        source: std::io::Error,
    },
}

/// What keeps one request from reaching Garage, or its answer from coming back: answered 502.
#[derive(Debug, thiserror::Error)]
enum ForwardError {
    #[error("reading an upload's content type")]
    ContentType(#[source] header::ToStrError),
    #[error("an upload that isn't a form: {content_type:?}")]
    NotAForm { content_type: String },
    #[error("reading the upload's length")]
    LengthHeader(#[source] header::ToStrError),
    #[error("the upload's length {length:?}")]
    Length {
        length: String,
        #[source]
        source: std::num::ParseIntError,
    },
    /// The form's length with the bucket field added, past what a usize holds.
    #[error("an upload of {length} bytes and {field} more, past usize")]
    TooLong { length: usize, field: usize },
    #[error("the upstream host {host:?}")]
    Host {
        host: String,
        #[source]
        source: axum::http::header::InvalidHeaderValue,
    },
    #[error("{method} {url}")]
    Garage {
        method: Method,
        url: String,
        #[source]
        source: reqwest::Error,
    },
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("cdn0: {}", testbox_tools::Report(&*error));
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let credentials_path = KeyFile::Cdn0.path();
    let credentials: Cdn0Credentials = toml::from_str(&testbox_tools::read(&credentials_path)?)
        .map_err(|source| Cdn0Error::Credentials {
            path: credentials_path,
            source,
        })?;
    write_config().await?;
    // Garage, once it says all its servers listen; its log goes on into ours.
    let (mut garage, heard) = spawn_logged(Command::new("garage").args(["-c", CONFIG, "server"]))?;
    wait_for(heard, GARAGE_READY, GARAGE_PATIENCE)
        .await
        .map_err(Cdn0Error::GarageListening)?;
    set_up(&credentials).await?;
    // The one provider built in (Cargo.toml).
    tls::install_provider()?;
    let tls = axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(tls::server_config(&[
        "http/1.1",
    ])?));
    tls::say_where_secrets_go("cdn0");
    // Until either fails, or SIGTERM stops Garage: as a container's first process, nothing else
    // ends the front.
    tokio::select! {
        served = serve(tls) => Ok(served?),
        supervised = supervise(&mut garage, "garage") => Ok(supervised?),
    }
}

async fn write_config() -> Result<(), Cdn0Error> {
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
    let text = toml::to_string(&config).map_err(Cdn0Error::SerializeConfig)?;
    tokio::fs::write(CONFIG, text)
        .await
        .map_err(|source| Cdn0Error::WriteConfig {
            path: CONFIG.into(),
            source,
        })
}

async fn garage(args: &[&str]) -> Result<String, CommandError> {
    output_async(Command::new("garage").args(["-c", CONFIG]).args(args)).await
}

/// Whether a listing shows `word` as a word of its own.
fn lists(listing: &str, word: &str) -> bool {
    listing.split_whitespace().any(|listed| listed == word)
}

/// Layout, key and bucket, each only if missing: /data outlives the container.
async fn set_up(credentials: &Cdn0Credentials) -> Result<(), CommandError> {
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

async fn serve(tls: axum_server::tls_rustls::RustlsConfig) -> Result<(), Cdn0Error> {
    let client = reqwest::Client::new();
    let app = axum::Router::new().fallback(move |request: Request| {
        let client = client.clone();
        async move { forward(&client, request).await }
    });
    eprintln!("CDN0 on {}", keys::localhost(keys::CDN0_PORT));
    let address = SocketAddr::from(([127, 0, 0, 1], keys::CDN0_PORT));
    axum_server::bind_rustls(address, tls)
        .serve(app.into_make_service())
        .await
        .map_err(|source| Cdn0Error::Serve { address, source })
}

/// One request to Garage, its answer back; what goes wrong on the way answers 502. Each is
/// logged as `cdn0: <method> <path> <status>`, which the checks wait on (a sticker downloaded).
async fn forward(client: &reqwest::Client, request: Request) -> Response {
    let method = request.method().to_string();
    let path = request.uri().path().to_owned();
    let response = match forward_to_garage(client, request).await {
        Ok(response) => response,
        Err(error) => {
            let report = testbox_tools::Report(&error).to_string();
            eprintln!("cdn0: {method} {path}: {report}");
            (StatusCode::BAD_GATEWAY, report).into_response()
        }
    };
    eprintln!(
        "{}",
        testbox_tools::cdn0_request_line(&method, &path, response.status().as_u16())
    );
    response
}

async fn forward_to_garage(
    client: &reqwest::Client,
    request: Request,
) -> Result<Response, ForwardError> {
    let (mut parts, body) = request.into_parts();
    let (url, host, body) = match parts.method {
        Method::POST if parts.uri.path() == "/" => {
            let prefix = bucket_field(&parts.headers)?;
            // The form grows by the field.
            if let Some(length) = parts.headers.get(header::CONTENT_LENGTH) {
                let length = length.to_str().map_err(ForwardError::LengthHeader)?;
                let length: usize = length.parse().map_err(|source| ForwardError::Length {
                    length: length.to_owned(),
                    source,
                })?;
                let length =
                    length
                        .checked_add(prefix.len())
                        .ok_or_else(|| ForwardError::TooLong {
                            length,
                            field: prefix.len(),
                        })?;
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
    let host_value =
        HeaderValue::from_str(&host).map_err(|source| ForwardError::Host { host, source })?;
    headers.insert(header::HOST, host_value);
    let answer = client
        .request(parts.method.clone(), &url)
        .headers(headers)
        .body(body)
        .send()
        .await
        .map_err(|source| ForwardError::Garage {
            method: parts.method,
            url,
            source,
        })?;
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
fn bucket_field(headers: &HeaderMap) -> Result<Bytes, ForwardError> {
    let content_type = match headers.get(header::CONTENT_TYPE) {
        Some(value) => value.to_str().map_err(ForwardError::ContentType)?,
        None => "",
    };
    let boundary = content_type
        .split(';')
        .find_map(|parameter| parameter.trim().strip_prefix("boundary="))
        .map(|boundary| boundary.trim_matches('"'))
        .ok_or_else(|| ForwardError::NotAForm {
            content_type: content_type.to_owned(),
        })?;
    Ok(Bytes::from(format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"bucket\"\r\n\r\n{}\r\n",
        keys::CDN0_BUCKET.name
    )))
}
