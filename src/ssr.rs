//! Server-side rendering.

#[cfg(feature = "ssr")]
use std::collections::HashSet;
use std::future::Future;
#[cfg(feature = "ssr")]
use std::path::PathBuf;
#[cfg(feature = "ssr")]
use std::sync::{LazyLock, Mutex, OnceLock, PoisonError};
#[cfg(feature = "ssr")]
use std::time::Duration;

use futures_util::future::BoxFuture;

#[cfg(feature = "ssr")]
use crate::files;
use crate::page::Page;
use crate::request::Request;

/// Server-side rendered HTML for a page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rendered {
    /// The `<head>` elements, such as the `<title>` set with `<Head>`.
    pub head: String,
    /// The app element with its rendered HTML, and the page data `<script>`.
    pub body: String,
}

/// Renders pages on the server.
///
/// Returning `None` falls back to client-side rendering, so a gateway that
/// can't reach its renderer should log and return `None` rather than fail.
pub trait Gateway: Send + Sync + 'static {
    /// Render a page.
    fn dispatch(&self, page: &Page, request: &Request) -> impl Future<Output = Option<Rendered>> + Send;
}

/// An object-safe [`Gateway`].
pub(crate) trait DynGateway: Send + Sync {
    fn dispatch<'a>(&'a self, page: &'a Page, request: &'a Request) -> BoxFuture<'a, Option<Rendered>>;
}

impl<G: Gateway> DynGateway for G {
    fn dispatch<'a>(&'a self, page: &'a Page, request: &'a Request) -> BoxFuture<'a, Option<Rendered>> {
        Box::pin(Gateway::dispatch(self, page, request))
    }
}

/// Renders pages through an Inertia SSR server over HTTP.
///
/// While the Vite dev server is running (its hot file exists), pages are
/// rendered by the `@inertiajs/vite` plugin's `/__inertia_ssr` endpoint.
/// Otherwise they're sent to the production SSR server's `/render` endpoint,
/// started with `node bootstrap/ssr/app.js`.
#[cfg(feature = "ssr")]
#[derive(Debug, Clone)]
#[must_use]
pub struct HttpGateway {
    url: String,
    hot_file: Option<PathBuf>,
    bundle: Option<PathBuf>,
    watch: bool,
    except: Vec<String>,
    enabled: bool,
    client: reqwest::Client,
}

#[cfg(feature = "ssr")]
impl Default for HttpGateway {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "ssr")]
impl HttpGateway {
    /// A gateway for the SSR server at `http://127.0.0.1:13714`.
    pub fn new() -> Self {
        Self {
            url: "http://127.0.0.1:13714".to_owned(),
            hot_file: None,
            bundle: None,
            watch: true,
            except: Vec::new(),
            enabled: true,
            client: default_client(),
        }
    }

    /// Set the production SSR server URL.
    pub fn url(mut self, url: impl Into<String>) -> Self {
        self.url = url.into().trim_end_matches('/').to_owned();
        self
    }

    /// Render through the Vite dev server while this hot file exists. The
    /// file holds the dev server's URL.
    pub fn hot_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.hot_file = Some(path.into());
        self
    }

    /// Only use the production SSR server when this bundle has been built.
    pub fn bundle(mut self, path: impl Into<PathBuf>) -> Self {
        self.bundle = Some(path.into());
        self
    }

    /// Set whether the hot file and the bundle are checked again while running.
    ///
    /// On by default, so starting the dev server or building the bundle takes
    /// effect without a restart: each is checked at most once a second. Turn
    /// it off in production, where neither changes without a deploy, to check
    /// each once.
    pub fn watch(mut self, watch: bool) -> Self {
        self.watch = watch;
        self
    }

    /// Never server-side render these paths. A trailing `*` matches any suffix.
    pub fn except<I, P>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<String>,
    {
        self.except.extend(paths.into_iter().map(Into::into));
        self
    }

    /// Enable or disable server-side rendering.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Set the request timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.client = client(timeout);
        self
    }

    /// Whether the production SSR server is up.
    pub async fn is_healthy(&self) -> bool {
        self.client
            .get(format!("{}/health", self.url))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
    }

    /// The URL to render with, or `None` when SSR is unavailable.
    fn endpoint(&self, request: &Request) -> Option<String> {
        if !self.enabled || self.is_excluded(request.path()) {
            return None;
        }

        if let Some(hot) = self.hot_file.as_ref().and_then(|path| files::read(path, self.watch)) {
            return Some(format!("{}/__inertia_ssr", hot.trim().trim_end_matches('/')));
        }

        if self
            .bundle
            .as_ref()
            .is_some_and(|bundle| !files::exists(bundle, self.watch))
        {
            return None;
        }

        Some(format!("{}/render", self.url))
    }

    fn is_excluded(&self, path: &str) -> bool {
        let path = path.trim_start_matches('/');

        self.except.iter().any(|pattern| {
            let pattern = pattern.trim_start_matches('/');

            match pattern.strip_suffix('*') {
                Some(prefix) => path.starts_with(prefix),
                None => path == pattern,
            }
        })
    }
}

#[cfg(feature = "ssr")]
impl Gateway for HttpGateway {
    async fn dispatch(&self, page: &Page, request: &Request) -> Option<Rendered> {
        #[derive(serde::Deserialize)]
        struct Response {
            #[serde(default)]
            head: Vec<String>,
            #[serde(default)]
            body: String,
        }

        let url = self.endpoint(request)?;

        let response = match self.client.post(&url).json(page).send().await {
            Ok(response) => response,
            Err(error) => {
                // Warn once: an SSR server that isn't running is a setup
                // issue, not something to repeat on every request.
                if unreachable().insert(url.clone()) {
                    tracing::warn!(%url, %error, "the Inertia SSR server is unreachable; rendering on the client");
                } else {
                    tracing::debug!(%url, %error, "the Inertia SSR server is unreachable");
                }
                return None;
            }
        };

        // It's back, so warn again if it goes down again.
        unreachable().remove(&url);

        if !response.status().is_success() {
            let status = response.status();
            let error = response.text().await.unwrap_or_default();
            tracing::error!(%url, %status, %error, "Inertia SSR failed; rendering on the client");
            return None;
        }

        // The Vite plugin responds with `null` while it warms up.
        match response.json::<Option<Response>>().await {
            Ok(rendered) => rendered.map(|Response { head, body }| Rendered {
                head: head.join("\n"),
                body,
            }),
            Err(error) => {
                tracing::error!(%url, %error, "invalid Inertia SSR response; rendering on the client");
                None
            }
        }
    }
}

/// The SSR servers found unreachable and warned about, until they're back.
///
/// Kept for the process rather than per gateway, since apps may build a
/// gateway per request. By URL, so each server's outage is warned about.
#[cfg(feature = "ssr")]
fn unreachable() -> std::sync::MutexGuard<'static, HashSet<String>> {
    static UNREACHABLE: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Mutex::default);

    UNREACHABLE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The client shared by gateways with the default timeout. Clients pool
/// connections, so building one per gateway would waste them.
#[cfg(feature = "ssr")]
fn default_client() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

    CLIENT.get_or_init(|| client(Duration::from_secs(5))).clone()
}

#[cfg(feature = "ssr")]
fn client(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .expect("the SSR HTTP client should build")
}
