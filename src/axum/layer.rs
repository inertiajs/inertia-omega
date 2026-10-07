use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::task::{Context, Poll};

use ::axum::body::Body;
use ::axum::extract::OriginalUri;
use ::axum::response::{IntoResponse, Response};
use futures_util::future::BoxFuture;
use http::StatusCode;
use http::request::Parts;
use http_body::Body as _;
use tower_layer::Layer;
use tower_service::Service;

use super::response::render;
use crate::config::Config;
use crate::inertia::Inertia;
use crate::props::PropError;
use crate::protocol;
use crate::request::Request;
use crate::session::{Session, SharedSession};

type ResolveSession = dyn Fn(&Parts) -> Option<SharedSession> + Send + Sync;

/// Installs Inertia on a router.
///
/// For each request it makes the [`Inertia`] extractor available, renders
/// the returned page, writes flash data and validation errors to the
/// session, and applies the protocol rules in [`protocol`].
#[derive(Clone)]
pub struct InertiaLayer {
    config: Arc<Config>,
    session: Arc<ResolveSession>,
}

impl InertiaLayer {
    /// A layer with the given configuration.
    pub fn new(config: Config) -> Self {
        Self {
            config: Arc::new(config),
            session: Arc::new(default_session),
        }
    }

    /// Read the session from each request with the given function, instead
    /// of using `tower_sessions::Session`.
    pub fn session<S, F>(mut self, resolve: F) -> Self
    where
        S: Session,
        F: Fn(&Parts) -> Option<S> + Send + Sync + 'static,
    {
        self.session = Arc::new(move |parts| resolve(parts).map(|session| Arc::new(session) as SharedSession));
        self
    }
}

impl InertiaLayer {
    /// Run Inertia around `next`, the rest of the request's handling.
    ///
    /// The layer calls this for each request; it's public so Inertia can
    /// be composed into other middleware systems, such as a framework's own.
    /// A prop that fails to resolve is logged, and the response is a `500`.
    pub async fn handle<F, Fut, E>(&self, request: http::Request<Body>, next: F) -> Result<Response, E>
    where
        F: FnOnce(http::Request<Body>) -> Fut,
        Fut: Future<Output = Result<Response, E>>,
    {
        self.handle_with(request, next, |error| {
            tracing::error!(%error, "failed to resolve Inertia props");
            server_error()
        })
        .await
    }

    /// Run Inertia around `next`, as [`handle`](Self::handle) does, but
    /// respond to a prop's failure with `failed`, for a framework to render
    /// it as it renders any error. Its response may be a page render, such
    /// as an error page; should that fail too, it's logged, and the
    /// response is a `500`.
    pub async fn handle_with<F, Fut, E>(
        &self,
        request: http::Request<Body>,
        next: F,
        failed: impl FnOnce(PropError) -> Response,
    ) -> Result<Response, E>
    where
        F: FnOnce(http::Request<Body>) -> Fut,
        Fut: Future<Output = Result<Response, E>>,
    {
        let (mut parts, body) = request.into_parts();

        // Nested routers strip their prefix from the URI; the page URL
        // should be the one the browser visited.
        let uri = parts
            .extensions
            .get::<OriginalUri>()
            .map_or(&parts.uri, |original| &original.0);
        let request = Request::new(parts.method.clone(), uri, parts.headers.clone());
        let inertia = Inertia::build(Arc::clone(&self.config), request, (self.session)(&parts));

        // Through the handle, so the version the check computes is the one
        // the page gets, not computed a second time.
        if let Some(response) = protocol::version_conflict(inertia.request(), || inertia.version()) {
            return Ok(response.map(Body::from));
        }

        parts.extensions.insert(inertia.clone());

        let response = next(http::Request::from_parts(parts, body)).await?;
        let response = match render(response).await {
            Ok(response) => response,
            Err(error) => render(failed(error)).await.unwrap_or_else(|error| {
                tracing::error!(%error, "failed to resolve the Inertia props of a failure's response");
                server_error()
            }),
        };

        inertia.commit().await;

        let (mut parts, body) = response.into_parts();
        let is_empty = body.size_hint().exact() == Some(0);

        Ok(match protocol::after(inertia.request(), &mut parts, is_empty) {
            Some(replacement) => replacement.map(Body::from),
            None => Response::from_parts(parts, body),
        })
    }
}

fn server_error() -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
}

fn default_session(parts: &Parts) -> Option<SharedSession> {
    #[cfg(feature = "tower-sessions")]
    if let Some(session) = parts.extensions.get::<tower_sessions::Session>() {
        return Some(Arc::new(session.clone()));
    }

    let _ = parts;
    None
}

impl<S> Layer<S> for InertiaLayer {
    type Service = InertiaService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        InertiaService {
            inner,
            layer: self.clone(),
        }
    }
}

impl fmt::Debug for InertiaLayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InertiaLayer")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

/// The service produced by an [`InertiaLayer`].
#[derive(Clone, Debug)]
pub struct InertiaService<S> {
    inner: S,
    layer: InertiaLayer,
}

impl<S> Service<http::Request<Body>> for InertiaService<S>
where
    S: Service<http::Request<Body>, Response = Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Response, S::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: http::Request<Body>) -> Self::Future {
        // Call the service that was polled ready, leaving a fresh clone behind.
        let ready = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, ready);
        let layer = self.layer.clone();

        Box::pin(async move { layer.handle(request, |request| inner.call(request)).await })
    }
}
