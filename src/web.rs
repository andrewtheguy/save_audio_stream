//! The web UI, compiled into the binary.
//!
//! `build.rs` writes the vite bundle to Cargo's `OUT_DIR` and every file in it
//! becomes bytes in the executable, so `save_audio_stream` is one file wherever
//! it runs: no `share/.../web` directory to install beside it, no environment
//! variable to point at one, and no "the UI will 404" warning at start-up — the
//! build refuses to finish without `index.html`.
//!
//! Vite names every asset by its content hash and only `index.html` keeps a
//! stable name, so each embedded file's hash is also its `ETag`: a browser that
//! already holds an asset revalidates it for a 304 instead of downloading it
//! again, and a redeployed server with a changed index answers with a fresh
//! document.
//!
//! The app routes with `HashRouter` (`frontend/src/main.tsx`), so client-side
//! routes live after the `#` and never reach the server: there is no SPA rewrite
//! here and an unknown path is honestly a 404.

use std::fmt::Write as _;

use axum::{
    body::Body,
    extract::Request,
    http::{HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use rust_embed::{EmbeddedFile, RustEmbed};

#[derive(RustEmbed)]
#[folder = "$OUT_DIR/frontend-dist"]
struct Frontend;

const INDEX: &str = "index.html";

/// Serve the bundle: `/` as `index.html`, any other path as the file of that
/// name, and a 404 for anything the bundle does not contain. This is the
/// router's fallback service, so only paths no API route claimed arrive here.
pub async fn serve(request: Request) -> Response {
    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let path = request.uri().path().trim_start_matches('/');
    let path = if path.is_empty() { INDEX } else { path };
    let Some(file) = Frontend::get(path) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let etag = etag(&file);
    if holds(request.headers().get(header::IF_NONE_MATCH), &etag) {
        return ([(header::ETAG, etag)], StatusCode::NOT_MODIFIED).into_response();
    }
    (
        [
            (header::CONTENT_TYPE, content_type(&file)),
            (header::ETAG, etag),
        ],
        Body::from(file.data),
    )
        .into_response()
}

/// Does an `If-None-Match` hold the file we are about to send? The header is
/// `*` or a comma-separated list of validators, and a read compares them
/// weakly: a `W/` prefix on either side does not stop a match, so a cache that
/// weakened our tag still revalidates to a 304 instead of re-downloading the
/// asset.
fn holds(header: Option<&HeaderValue>, etag: &HeaderValue) -> bool {
    let Some(held) = header.and_then(|held| held.to_str().ok()) else {
        return false;
    };
    let etag = etag.to_str().expect("our own tag is hex digits and quotes");
    held.trim() == "*"
        || held.split(',').any(|candidate| {
            let candidate = candidate.trim();
            candidate.strip_prefix("W/").unwrap_or(candidate) == etag
        })
}

/// A strong validator from the file's content hash, quoted as the header wants.
fn etag(file: &EmbeddedFile) -> HeaderValue {
    let mut tag = String::with_capacity(66);
    tag.push('"');
    for byte in file.metadata.sha256_hash() {
        write!(tag, "{byte:02x}").expect("writing to a String cannot fail");
    }
    tag.push('"');
    HeaderValue::from_str(&tag).expect("hex digits and quotes are a valid header value")
}

/// The content type from the file's extension. Vite writes UTF-8, and a text
/// type says so: a browser told `text/html` alone may guess a legacy encoding.
fn content_type(file: &EmbeddedFile) -> HeaderValue {
    let mime = file.metadata.mimetype();
    let value = if mime.starts_with("text/") || mime == "application/javascript" {
        format!("{mime}; charset=utf-8")
    } else {
        mime.to_owned()
    };
    HeaderValue::from_str(&value).expect("mime_guess returns header-safe types")
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn get(path: &str, if_none_match: Option<&HeaderValue>) -> Response {
        let mut request = Request::builder().uri(path);
        if let Some(held) = if_none_match {
            request = request.header(header::IF_NONE_MATCH, held);
        }
        serve(request.body(Body::empty()).unwrap()).await
    }

    async fn body(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 24)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    /// The bundle vite wrote is what is served: the document at `/`, and the
    /// hashed assets it references beside it.
    #[tokio::test]
    async fn the_document_and_its_assets_are_in_the_binary() {
        let response = get("/", None).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
        let index = body(response).await;
        assert!(index.contains("<div id=\"root\">"), "{index}");

        let script = Frontend::iter()
            .find(|name| name.starts_with("assets/") && name.ends_with(".js"))
            .expect("the bundle has a script");
        let response = get(&format!("/{script}"), None).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/javascript; charset=utf-8"
        );
        let stylesheet = Frontend::iter()
            .find(|name| name.ends_with(".css"))
            .expect("the bundle has a stylesheet");
        let response = get(&format!("/{stylesheet}"), None).await;
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/css; charset=utf-8"
        );
    }

    /// A path that is not a file in the bundle is a 404: routing lives after the
    /// `#`, so nothing legitimate ever asks the server for another document.
    #[tokio::test]
    async fn paths_outside_the_bundle_are_not_found() {
        for path in [
            "/login",
            "/assets/",
            "/assets/../index.html",
            "/no/such/thing",
        ] {
            let response = get(path, None).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
    }

    /// The `ETag` is the content hash, so a browser holding the file gets a 304
    /// for it and a full answer once the file has changed.
    #[tokio::test]
    async fn a_held_file_revalidates_to_not_modified() {
        let first = get("/", None).await;
        let etag = first.headers()[header::ETAG].clone();
        assert!(etag.to_str().unwrap().starts_with('"'), "{etag:?}");

        let revalidated = get("/", Some(&etag)).await;
        assert_eq!(revalidated.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(revalidated.headers()[header::ETAG], etag);
        assert!(body(revalidated).await.is_empty());

        let stale = get("/", Some(&HeaderValue::from_static("\"something-else\""))).await;
        assert_eq!(stale.status(), StatusCode::OK);
    }

    /// The header is a list, not one tag: `*`, a weakened copy of our tag, and
    /// our tag among others all mean the browser already holds this file.
    #[tokio::test]
    async fn every_form_of_if_none_match_is_honoured() {
        let etag = get("/", None).await.headers()[header::ETAG]
            .to_str()
            .unwrap()
            .to_owned();

        for held in [
            "*".to_owned(),
            format!("W/{etag}"),
            format!("\"other\", {etag}"),
            format!("{etag} , \"other\""),
        ] {
            let response = get("/", Some(&HeaderValue::from_str(&held).unwrap())).await;
            assert_eq!(response.status(), StatusCode::NOT_MODIFIED, "{held}");
        }

        for held in ["\"other\", W/\"another\"", "\"\"", ""] {
            let response = get("/", Some(&HeaderValue::from_static(held))).await;
            assert_eq!(response.status(), StatusCode::OK, "{held}");
        }
    }

    /// Nothing here takes a body: the page is read, never written to.
    #[tokio::test]
    async fn only_reads_are_answered() {
        let request = Request::builder()
            .method(Method::POST)
            .uri("/")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            serve(request).await.status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
    }
}
