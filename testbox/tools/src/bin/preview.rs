//! The page link previews are made of, as a site would serve it: `/page/<n>`, with its title,
//! description and picture as `og:` tags, and the picture at `/picture.png`. Over TLS with the
//! keys' certificate, which names [`keys::PREVIEW_HOST`], at [`keys::PREVIEW_ADDRESS`] on the
//! pod's loopback (testbox/pod.yaml). Each request is logged as `preview: GET <path> <status>`.

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

use std::net::SocketAddr;

use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use testbox_tools::keys::{self, KeyFile};
use testbox_tools::{Context, Error, Result};

const PICTURE: &[u8] = include_bytes!("../../assets/preview.png");

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    testbox_tools::exit("preview", serve().await)
}

async fn serve() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_installed| Error::Failed("a TLS provider is installed already".to_owned()))?;
    let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(
        KeyFile::TlsCertificate.path(),
        KeyFile::TlsKey.path(),
    )
    .await
    .context("the keys' certificate")?;
    let app = axum::Router::new()
        .route("/page/{n}", get(page))
        .route("/picture.png", get(picture))
        .fallback(|uri: axum::http::Uri| async move {
            logged(uri.path(), StatusCode::NOT_FOUND.into_response())
        });
    let address = SocketAddr::from((keys::PREVIEW_ADDRESS, keys::PREVIEW_PORT));
    let listener = axum_server::bind_rustls(address, tls);
    eprintln!(
        "{} on {}",
        testbox_tools::PREVIEW_READY,
        keys::preview_url("/")
    );
    listener
        .serve(app.into_make_service())
        .await
        .context("serving the preview page")
}

async fn page(Path(n): Path<u32>) -> Response {
    let title = testbox_tools::preview_title(n);
    let picture = keys::preview_url("/picture.png");
    let html = format!(
        r#"<!doctype html>
<html><head>
<meta charset="utf-8">
<title>{title}</title>
<meta property="og:title" content="{title}">
<meta property="og:description" content="A page served inside the box">
<meta property="og:image" content="{picture}">
</head><body><p>{title}</p></body></html>
"#
    );
    logged(&format!("/page/{n}"), Html(html).into_response())
}

async fn picture() -> Response {
    logged(
        "/picture.png",
        ([(header::CONTENT_TYPE, "image/png")], PICTURE).into_response(),
    )
}

fn logged(path: &str, response: Response) -> Response {
    eprintln!(
        "{}",
        testbox_tools::preview_request_line(path, response.status().as_u16())
    );
    response
}
