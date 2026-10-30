//! A page render, resolved into an HTTP response once the handler returns.

use std::fmt;

use http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::HttpResponse;
use crate::errors::ErrorBags;
use crate::header as inertia_header;
use crate::inertia::Inertia;
use crate::json::{encode_big_integers, escape_attribute, html_safe_json};
use crate::page::Page;
use crate::props::{self, IntoProp, PropError, Props, PropsResolver};
use crate::session::{SharedSession, key};
use crate::ssr::Rendered;
use crate::view::View;

/// A page render.
///
/// Return it from a handler: the adapter resolves the props and builds the
/// HTML document or JSON page once the handler is done.
/// Use [`Response::into_http`] to do so directly.
///
/// ```no_run
/// # use inertia::{Inertia, props};
/// # fn handler(inertia: Inertia) -> inertia::Response {
/// inertia
///     .render("Dashboard", props! { stats => [1, 2, 3] })
///     .with("title", "Dashboard")
///     .clear_history()
/// # }
/// ```
#[must_use = "a page render does nothing unless it is returned or turned into an HTTP response"]
pub struct Response {
    inertia: Inertia,
    component: String,
    props: Result<Props, PropError>,
    view_data: Map<String, Value>,
    flash: Map<String, Value>,
    encrypt_history: Option<bool>,
    clear_history: bool,
    preserve_big_integers: Option<bool>,
    ssr: bool,
    headers: HeaderMap,
}

impl Response {
    pub(crate) fn new(inertia: Inertia, component: String, props: Result<Props, PropError>) -> Self {
        Self {
            inertia,
            component,
            props,
            view_data: Map::new(),
            flash: Map::new(),
            encrypt_history: None,
            clear_history: false,
            preserve_big_integers: None,
            ssr: true,
            headers: HeaderMap::new(),
        }
    }

    /// Add a prop.
    pub fn with(mut self, key: impl Into<String>, value: impl IntoProp) -> Self {
        if let Ok(props) = &mut self.props {
            props.insert(key, value);
        }
        self
    }

    /// Add data for the root view only, which the page doesn't receive.
    pub fn with_view_data(mut self, key: impl Into<String>, value: impl Serialize) -> Self {
        self.view_data
            .insert(key.into(), serde_json::to_value(value).unwrap_or_default());
        self
    }

    /// Flash data to this page.
    pub fn flash(mut self, key: impl Into<String>, value: impl Serialize) -> Self {
        self.flash
            .insert(key.into(), serde_json::to_value(value).unwrap_or_default());
        self
    }

    /// Send a header with the page, whether it's sent as JSON or as a whole
    /// document. It replaces any of the same name.
    ///
    /// # Panics
    ///
    /// Panics when the name or value isn't a valid header.
    #[track_caller]
    pub fn with_header<K, V>(mut self, name: K, value: V) -> Self
    where
        K: TryInto<HeaderName>,
        K::Error: fmt::Debug,
        V: TryInto<HeaderValue>,
        V::Error: fmt::Debug,
    {
        let name = name.try_into().expect("a valid header name");
        let value = value.try_into().expect("a valid header value");

        self.headers.insert(name, value);
        self
    }

    /// Send several headers with the page.
    ///
    /// # Panics
    ///
    /// Panics when a name or value isn't a valid header.
    #[track_caller]
    pub fn with_headers<K, V>(mut self, headers: impl IntoIterator<Item = (K, V)>) -> Self
    where
        K: TryInto<HeaderName>,
        K::Error: fmt::Debug,
        V: TryInto<HeaderValue>,
        V::Error: fmt::Debug,
    {
        for (name, value) in headers {
            self = self.with_header(name, value);
        }
        self
    }

    /// Encrypt this page in the browser history.
    pub fn encrypt_history(mut self, encrypt: bool) -> Self {
        self.encrypt_history = Some(encrypt);
        self
    }

    /// Clear the browser history when this page is visited.
    pub fn clear_history(mut self) -> Self {
        self.clear_history = true;
        self
    }

    /// Send integers outside JavaScript's safe range as `BigInt`s.
    pub fn preserve_big_integers(mut self, preserve: bool) -> Self {
        self.preserve_big_integers = Some(preserve);
        self
    }

    /// Render this page on the client, even when SSR is enabled.
    pub fn without_ssr(mut self) -> Self {
        self.ssr = false;
        self
    }

    /// Resolve the props and build the HTTP response: JSON for Inertia
    /// visits, the root view's HTML document otherwise. A prop that fails
    /// to resolve is logged, and the response is a `500`.
    pub async fn into_http(self) -> HttpResponse {
        self.try_into_http().await.unwrap_or_else(|error| {
            tracing::error!(%error, "failed to resolve Inertia props");
            text(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
        })
    }

    /// Resolve the props and build the HTTP response, as [`into_http`](Self::into_http)
    /// does, but return a prop's failure, for a framework to render as it
    /// renders any error.
    ///
    /// # Errors
    ///
    /// Fails when the props didn't serialize to an object, or a prop that
    /// isn't [rescued](crate::Prop::rescue) failed to resolve.
    pub async fn try_into_http(mut self) -> Result<HttpResponse, PropError> {
        let inertia = self.inertia.clone();
        let view_data = std::mem::take(&mut self.view_data);
        let headers = std::mem::take(&mut self.headers);
        let ssr = self.ssr;
        let page = self.into_page().await?;

        let mut response = if inertia.request().is_inertia() {
            json(&page)
        } else {
            document(&inertia, &page, &view_data, ssr).await
        };

        for (name, value) in &headers {
            response.headers_mut().insert(name, value.clone());
        }

        Ok(response)
    }

    /// Resolve the props into the page object.
    ///
    /// # Errors
    ///
    /// Fails when the props didn't serialize to an object, or a prop that
    /// isn't [rescued](crate::Prop::rescue) failed to resolve.
    pub async fn into_page(self) -> Result<Page, PropError> {
        let Self {
            inertia,
            component,
            props,
            flash,
            encrypt_history,
            clear_history,
            preserve_big_integers,
            ..
        } = self;
        let props = props?;
        let config = inertia.config();
        let request = inertia.request();
        let mut pending = inertia.take_pending();
        let stored = Stored::pull(inertia.session()).await;

        let mut errors = stored.errors.clone();
        errors.merge(pending.errors.clone());

        let mut shared = Props::new();
        shared.insert(
            "errors",
            props::always(errors.to_prop(request.error_bag(), config.all_errors)),
        );
        for share in &config.shares {
            shared.extend(share(request));
        }
        shared.extend(std::mem::take(&mut pending.shared));

        let resolved = PropsResolver::new(request, &component)
            .resolve(shared, props, config.expose_shared_prop_keys)
            .await;

        // Keep flash data, errors and history flags for the next render; shared props were consumed above...
        let (props, metadata) = match resolved {
            Ok(resolved) => resolved,
            Err(error) => {
                inertia.restore_pending(pending);
                stored.restore(inertia.session()).await;
                return Err(error);
            }
        };

        let preserve_big_integers = preserve_big_integers.unwrap_or(config.preserve_big_integers);
        let encode = |map: Map<String, Value>| {
            if !preserve_big_integers {
                return map;
            }

            match encode_big_integers(Value::Object(map)) {
                Value::Object(map) => map,
                _ => unreachable!("an object stays an object"),
            }
        };

        let mut all_flash = stored.flash;
        all_flash.extend(pending.flash);
        all_flash.extend(inertia.take_flash());
        all_flash.extend(flash);

        Ok(Page {
            component,
            props: encode(props),
            url: config
                .url_resolver
                .as_ref()
                .map_or_else(|| request.url().to_owned(), |resolve| resolve(request)),
            version: inertia.version().to_owned(),
            metadata,
            preserve_big_integers,
            clear_history: clear_history || pending.clear_history || stored.clear_history,
            encrypt_history: encrypt_history
                .or(pending.encrypt_history)
                .unwrap_or(config.encrypt_history),
            flash: encode(all_flash),
            preserve_fragment: pending.preserve_fragment || stored.preserve_fragment,
        })
    }
}

/// What previous requests left in the session for this page.
#[derive(Default)]
struct Stored {
    flash: Map<String, Value>,
    errors: ErrorBags,
    clear_history: bool,
    preserve_fragment: bool,
    /// The values pulled from the session, as stored.
    pulled: Vec<(&'static str, Value)>,
}

impl Stored {
    /// Pull the stored state, so it's delivered to this page only.
    async fn pull(session: Option<&SharedSession>) -> Self {
        let Some(session) = session else {
            return Self::default();
        };

        let mut stored = Self::default();
        stored.flash = stored.pull_key(session, key::FLASH).await;
        stored.errors = stored.pull_key(session, key::ERRORS).await;
        stored.clear_history = stored.pull_key(session, key::CLEAR_HISTORY).await;
        stored.preserve_fragment = stored.pull_key(session, key::PRESERVE_FRAGMENT).await;
        stored
    }

    async fn pull_key<T: serde::de::DeserializeOwned + Default>(
        &mut self,
        session: &SharedSession,
        key: &'static str,
    ) -> T {
        let Some(value) = session.pull(key).await else {
            return T::default();
        };

        let parsed = serde_json::from_value(value.clone()).unwrap_or_default();
        self.pulled.push((key, value));
        parsed
    }

    /// Put the pulled state back, for the render after a failed one.
    async fn restore(self, session: Option<&SharedSession>) {
        let Some(session) = session else {
            return;
        };

        for (key, value) in self.pulled {
            session.put(key, value).await;
        }
    }
}

/// The page as JSON, for Inertia visits.
fn json(page: &Page) -> HttpResponse {
    match serde_json::to_string(page) {
        Ok(json) => {
            let mut response = HttpResponse::new(json);
            let headers = response.headers_mut();
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
            headers.insert(inertia_header::INERTIA, HeaderValue::from_static("true"));
            headers.insert(header::VARY, HeaderValue::from_static("X-Inertia"));
            response
        }
        Err(error) => {
            tracing::error!(%error, "failed to serialize the Inertia page");
            text(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
        }
    }
}

/// The root view's HTML document, for first visits.
async fn document(inertia: &Inertia, page: &Page, data: &Map<String, Value>, ssr: bool) -> HttpResponse {
    let config = inertia.config();

    let rendered = match (&config.gateway, ssr) {
        (Some(gateway), true) => gateway.dispatch(page, inertia.request()).await,
        _ => None,
    };

    let ssr = rendered.is_some();
    let Rendered { head, body } = rendered.unwrap_or_else(|| Rendered {
        head: String::new(),
        body: format!(
            r#"<script data-page="{id}" type="application/json">{json}</script><div id="{id}"></div>"#,
            id = escape_attribute(&config.root_id),
            json = html_safe_json(page),
        ),
    });

    let html = config.render_root_view(&View {
        page,
        head: &head,
        body: &body,
        data,
        ssr,
    });

    let mut response = HttpResponse::new(html);
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(header::VARY, HeaderValue::from_static("X-Inertia"));
    response
}

fn text(status: StatusCode, body: &str) -> HttpResponse {
    let mut response = HttpResponse::new(body.to_owned());
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}
