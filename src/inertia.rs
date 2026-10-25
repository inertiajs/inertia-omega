//! The per-request Inertia handle.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde::Serialize;
use serde_json::{Map, Value};

use crate::HttpResponse;
use crate::config::Config;
use crate::errors::{ErrorBags, ValidationErrors};
use crate::props::{IntoProp, IntoProps, Props};
use crate::protocol;
use crate::request::Request;
use crate::response::Response;
use crate::session::{Session, SharedSession, key};

/// Inertia, for the current request.
///
/// Adapters create one per request; with Axum it's an extractor. Clones are
/// cheap and share state, so props shared or data flashed through one clone
/// (in a middleware, say) apply to the response rendered through another.
///
/// Flash data, validation errors and history flags are queued during the
/// request and written to the session when it ends, for the next page render.
/// The next render may be in this same request.
///
/// ```no_run
/// # use inertia::{Inertia, props};
/// fn show(inertia: Inertia) -> inertia::Response {
///     inertia.render("Users/Show", props! { user => "Taylor" })
/// }
/// ```
#[derive(Clone)]
pub struct Inertia {
    context: Arc<Context>,
}

struct Context {
    config: Arc<Config>,
    request: Request,
    session: Option<SharedSession>,
    pending: Mutex<Pending>,
}

/// State queued for the next page render.
#[derive(Default)]
pub(crate) struct Pending {
    pub(crate) shared: Props,
    pub(crate) flash: Map<String, Value>,
    pub(crate) errors: ErrorBags,
    pub(crate) clear_history: bool,
    pub(crate) preserve_fragment: bool,
    pub(crate) encrypt_history: Option<bool>,
}

impl Inertia {
    /// Create a handle without a session. Queued data can reach this request's
    /// page, but can't carry over to later requests.
    pub fn new(config: impl Into<Arc<Config>>, request: Request) -> Self {
        Self::build(config.into(), request, None)
    }

    /// Create a handle for a request with a session.
    pub fn with_session(config: impl Into<Arc<Config>>, request: Request, session: impl Session) -> Self {
        Self::build(config.into(), request, Some(Arc::new(session)))
    }

    pub(crate) fn build(config: Arc<Config>, request: Request, session: Option<SharedSession>) -> Self {
        Self {
            context: Arc::new(Context {
                config,
                request,
                session,
                pending: Mutex::default(),
            }),
        }
    }

    /// The configuration.
    pub fn config(&self) -> &Config {
        &self.context.config
    }

    /// The request.
    pub fn request(&self) -> &Request {
        &self.context.request
    }

    /// Render a page component.
    ///
    /// `props` may be [`Props`], usually built with [`props!`](crate::props!),
    /// or any `Serialize` type that serializes to an object, such as a struct.
    pub fn render(&self, component: impl Into<String>, props: impl IntoProps) -> Response {
        Response::new(self.clone(), component.into(), props.into_props())
    }

    /// Share a prop with this request's page.
    pub fn share(&self, key: impl Into<String>, value: impl IntoProp) -> &Self {
        let key = key.into();
        // Convert before locking: application serialization may share more data...
        let value = value.into_prop();

        let replaced = self.pending().shared.0.insert(key, value);
        // Drop a replaced prop after the lock, since a callback's captures can run application code...
        drop(replaced);
        self
    }

    /// Flash data to the next page, in its `flash` field. Unlike props, flash
    /// data isn't kept in the browser history, which suits notifications.
    pub fn flash(&self, key: impl Into<String>, value: impl Serialize) -> &Self {
        let key = key.into();
        match serde_json::to_value(value) {
            Ok(value) => {
                self.pending().flash.insert(key, value);
            }
            Err(error) => tracing::error!(%error, "failed to serialize Inertia flash data"),
        }
        self
    }

    /// Share validation errors with the next page, in the default error bag.
    pub fn with_errors(&self, errors: impl Into<ValidationErrors>) -> &Self {
        self.with_errors_in("default", errors)
    }

    /// Share validation errors with the next page, in the given error bag.
    pub fn with_errors_in(&self, bag: impl Into<String>, errors: impl Into<ValidationErrors>) -> &Self {
        let bag = bag.into();
        let errors = errors.into();
        self.pending().errors.add(bag, errors);
        self
    }

    /// Clear the browser history on the next page, so encrypted pages can't be restored.
    pub fn clear_history(&self) -> &Self {
        self.pending().clear_history = true;
        self
    }

    /// Keep the URL fragment of the current visit across the next redirect.
    pub fn preserve_fragment(&self) -> &Self {
        self.pending().preserve_fragment = true;
        self
    }

    /// Encrypt this request's page in the browser history.
    pub fn encrypt_history(&self, encrypt: bool) -> &Self {
        self.pending().encrypt_history = Some(encrypt);
        self
    }

    /// Redirect back to the previous page (the `Referer`), or `/`.
    #[must_use = "a redirect does nothing unless it's returned"]
    pub fn back(&self) -> HttpResponse {
        protocol::redirect(self.request().referer().unwrap_or("/"))
    }

    /// Share validation errors with the next page and redirect back.
    #[must_use = "a redirect does nothing unless it's returned"]
    pub fn back_with_errors(&self, errors: impl Into<ValidationErrors>) -> HttpResponse {
        self.with_errors(errors);
        self.back()
    }

    /// Redirect to an external URL, or force a full page load. Inertia
    /// visits receive a `409 Conflict` with an `X-Inertia-Location` header.
    #[must_use = "a redirect does nothing unless it's returned"]
    pub fn location(&self, url: impl AsRef<str>) -> HttpResponse {
        protocol::location(self.request(), url.as_ref())
    }

    /// Write the queued flash data, validation errors and history flags to
    /// the session. Adapters call this once the handler and any page render
    /// have finished.
    pub async fn commit(&self) {
        let Pending {
            shared,
            flash,
            errors,
            clear_history,
            preserve_fragment,
            ..
        } = std::mem::take(&mut *self.pending());
        // Drop props no page rendered after the lock, since their callbacks' captures can run application code...
        drop(shared);

        let has_data = !flash.is_empty() || !errors.is_empty() || clear_history || preserve_fragment;

        let Some(session) = self.session() else {
            if has_data {
                tracing::warn!("Inertia flash data, errors and history flags require a session; they were discarded");
            }
            return;
        };

        if !flash.is_empty() {
            let mut stored: Map<String, Value> = stored(session, key::FLASH).await;
            stored.extend(flash);
            session.put(key::FLASH, Value::Object(stored)).await;
        }

        if !errors.is_empty() {
            let mut stored: ErrorBags = stored(session, key::ERRORS).await;
            stored.merge(errors);
            session
                .put(key::ERRORS, serde_json::to_value(stored).unwrap_or_default())
                .await;
        }

        if clear_history {
            session.put(key::CLEAR_HISTORY, Value::Bool(true)).await;
        }

        if preserve_fragment {
            session.put(key::PRESERVE_FRAGMENT, Value::Bool(true)).await;
        }
    }

    pub(crate) fn session(&self) -> Option<&SharedSession> {
        self.context.session.as_ref()
    }

    /// Take everything queued for the next page.
    pub(crate) fn take_pending(&self) -> Pending {
        std::mem::take(&mut *self.pending())
    }

    /// Take flash data queued while resolving props.
    pub(crate) fn take_flash(&self) -> Map<String, Value> {
        std::mem::take(&mut self.pending().flash)
    }

    fn pending(&self) -> MutexGuard<'_, Pending> {
        self.context.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Read a value stored by a previous request, or its default.
pub(crate) async fn stored<T: serde::de::DeserializeOwned + Default>(session: &SharedSession, key: &str) -> T {
    session
        .get(key)
        .await
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

impl fmt::Debug for Inertia {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Inertia")
            .field("request", self.request())
            .field("session", &self.session().is_some())
            .finish_non_exhaustive()
    }
}
