//! Serving the built frontend from disk.

use axum::Router;
use log::{info, warn};
use tower_http::services::ServeDir;

/// Attach the built frontend as the router's fallback.
///
/// `ServeDir` serves `index.html` for `/` and every other emitted file with a
/// content type guessed from its extension, so the bundle's file names are the
/// frontend's business — which is what lets vite go back to content-hashed
/// names.
///
/// The app routes with `HashRouter` (`frontend/src/main.tsx`), so client-side
/// routes live after the `#` and never reach the server: there is no SPA
/// rewrite to install here and an unknown path is honestly a 404.
///
/// A missing bundle is not fatal. The API still serves, which is what a
/// headless container and `--sync-only` want.
pub fn attach_static<S>(router: Router<S>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    match crate::paths::static_dir() {
        Some(dir) => {
            // Only SAVE_AUDIO_STREAM_WEB_DIR can name a directory that isn't
            // there — the other two candidates are checked before being
            // returned. Mount it anyway so the failure is a plain 404 rather
            // than a refusal to start, but say so: a silently empty web UI is
            // the kind of thing that gets debugged from the browser side for an
            // hour.
            if !dir.is_dir() {
                warn!(
                    "Web UI directory {} does not exist — the UI will 404 \
                     (SAVE_AUDIO_STREAM_WEB_DIR points there)",
                    dir.display()
                );
            } else if !dir.join("index.html").is_file() {
                warn!(
                    "No index.html in web UI directory {} — the UI will 404",
                    dir.display()
                );
            } else {
                info!("Serving web UI from {}", dir.display());
            }
            router.fallback_service(ServeDir::new(dir))
        }
        None => {
            warn!(
                "No web UI bundle found — serving the API only. \
                 Build it with `bun run --cwd frontend build`, \
                 or point SAVE_AUDIO_STREAM_WEB_DIR at one."
            );
            router
        }
    }
}
