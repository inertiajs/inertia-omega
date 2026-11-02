//! Assertions for testing Inertia pages.
//!
//! ```
//! use inertia::testing::AssertablePage;
//!
//! let body = r#"{"component":"Users/Index","props":{"users":[{"name":"Taylor"}]},"url":"/users","version":"1"}"#;
//!
//! AssertablePage::from_body(body)
//!     .component("Users/Index")
//!     .has_count("users", 1)
//!     .equals("users.0.name", "Taylor")
//!     .missing("password");
//! ```
//!
//! A page read by an adapter that knows how to send requests can also
//! [`reload`](AssertablePage::reload) itself, as the Inertia client does.

use std::fmt;
use std::future::Future;
use std::panic::Location;
use std::pin::Pin;
use std::sync::Arc;

use http::{HeaderMap, HeaderValue};
use serde::Serialize;
use serde_json::Value;

use crate::header;
use crate::page::Page;

/// A page under test. Each assertion panics with a descriptive message when it fails.
#[derive(Clone)]
pub struct AssertablePage {
    page: Page,
    reloader: Option<Reloader>,
}

type Reloader = Arc<dyn Fn(ReloadRequest) -> Pin<Box<dyn Future<Output = AssertablePage> + Send>> + Send + Sync>;

impl AssertablePage {
    /// Read the page from a response body: the JSON of an Inertia visit, or
    /// the HTML document of a first visit.
    #[track_caller]
    pub fn from_body(body: &str) -> Self {
        let json = if body.trim_start().starts_with('{') {
            body
        } else {
            page_script(body)
        };

        match serde_json::from_str(json) {
            Ok(page) => Self { page, reloader: None },
            Err(error) => panic!("the response body doesn't contain a valid Inertia page: {error}\n\n{body}"),
        }
    }

    /// The page.
    pub fn page(&self) -> &Page {
        &self.page
    }

    /// The prop at the given dot-notation path, such as `users.0.name`.
    pub fn prop(&self, path: &str) -> Option<&Value> {
        let mut segments = path.split('.');
        let mut value = self.page.props.get(segments.next()?)?;

        for segment in segments {
            value = match value {
                Value::Object(object) => object.get(segment)?,
                Value::Array(items) => items.get(segment.parse::<usize>().ok()?)?,
                _ => return None,
            };
        }

        Some(value)
    }

    /// Assert the page component. A failure shows the page's props, such as
    /// the `status` and `message` of an error page rendered instead.
    #[track_caller]
    pub fn component(&self, expected: &str) -> &Self {
        self.assert_field("page component", &self.page.component, expected)
    }

    /// Assert the page URL.
    #[track_caller]
    pub fn url(&self, expected: &str) -> &Self {
        self.assert_field("page URL", &self.page.url, expected)
    }

    /// Assert the asset version.
    #[track_caller]
    pub fn version(&self, expected: &str) -> &Self {
        self.assert_field("asset version", &self.page.version, expected)
    }

    #[track_caller]
    fn assert_field(&self, field: &str, actual: &str, expected: &str) -> &Self {
        assert!(
            actual == expected,
            "expected the Inertia {field} `{expected}`, but it's `{actual}`\n\n{}",
            self.props()
        );
        self
    }

    /// Assert that the prop exists.
    #[track_caller]
    pub fn has(&self, path: &str) -> &Self {
        assert!(
            self.prop(path).is_some(),
            "expected the Inertia page to have prop `{path}`\n\n{}",
            self.props()
        );
        self
    }

    /// Assert that the prop is an array or object with the given number of items.
    #[track_caller]
    pub fn has_count(&self, path: &str, count: usize) -> &Self {
        let actual = match self.prop(path) {
            Some(Value::Array(items)) => items.len(),
            Some(Value::Object(object)) => object.len(),
            Some(other) => panic!("expected Inertia prop `{path}` to be an array or object, got {other}"),
            None => panic!("expected the Inertia page to have prop `{path}`\n\n{}", self.props()),
        };

        assert_eq!(actual, count, "unexpected number of items in Inertia prop `{path}`");
        self
    }

    /// Assert that the prop doesn't exist.
    #[track_caller]
    pub fn missing(&self, path: &str) -> &Self {
        assert!(
            self.prop(path).is_none(),
            "expected the Inertia page not to have prop `{path}`\n\n{}",
            self.props()
        );
        self
    }

    /// Assert that the prop equals the given value.
    #[track_caller]
    pub fn equals(&self, path: &str, expected: impl Serialize) -> &Self {
        let expected = serde_json::to_value(expected).expect("the expected value should serialize");

        match self.prop(path) {
            Some(actual) => assert_eq!(actual, &expected, "unexpected value of Inertia prop `{path}`"),
            None => panic!("expected the Inertia page to have prop `{path}`\n\n{}", self.props()),
        }
        self
    }

    /// Assert that the given props are deferred in the given group.
    #[track_caller]
    pub fn deferred(&self, group: &str, props: &[&str]) -> &Self {
        let deferred = self
            .page
            .metadata
            .deferred_props
            .get(group)
            .map(Vec::as_slice)
            .unwrap_or_default();

        assert_eq!(deferred, props, "unexpected deferred props in group `{group}`");
        self
    }

    /// Assert the page's flash data.
    #[track_caller]
    pub fn flash(&self, key: &str, expected: impl Serialize) -> &Self {
        let expected = serde_json::to_value(expected).expect("the expected value should serialize");

        assert_eq!(
            self.page.flash.get(key),
            Some(&expected),
            "unexpected Inertia flash data `{key}`"
        );
        self
    }

    fn props(&self) -> String {
        serde_json::to_string_pretty(&self.page.props).unwrap_or_default()
    }

    /// Send this page's [reloads](Self::reload) with the given function, which
    /// sends a [`ReloadRequest`] to the application and reads the page it
    /// responds with. Adapters call this when they read a page from a
    /// response.
    ///
    /// ```no_run
    /// use inertia::testing::AssertablePage;
    ///
    /// # async fn send(url: &str, headers: &http::HeaderMap) -> String { unimplemented!() }
    /// # async fn test(body: &str) {
    /// let page = AssertablePage::from_body(body).reloads_with(|reload| async move {
    ///     AssertablePage::from_body(&send(reload.url(), reload.headers()).await)
    /// });
    ///
    /// page.reload_only(&["users"]).await.has("users");
    /// # }
    /// ```
    pub fn reloads_with<F, R>(mut self, reload: F) -> Self
    where
        F: Fn(ReloadRequest) -> R + Send + Sync + 'static,
        R: Future<Output = Self> + Send + 'static,
    {
        self.reloader = Some(Arc::new(move |request| Box::pin(reload(request))));
        self
    }

    /// Reload the page with an Inertia visit to its URL, then assert that the
    /// reloaded page has the same component, URL and asset version. Await it
    /// for the reloaded page.
    ///
    /// ```no_run
    /// # async fn test(page: inertia::testing::AssertablePage) {
    /// page.reload().await.equals("visits", 2);
    /// # }
    /// ```
    ///
    /// # Panics
    ///
    /// Panics when the page wasn't read by an adapter that can
    /// [send reloads](Self::reloads_with). The future panics when the
    /// reloaded page differs.
    #[track_caller]
    pub fn reload(&self) -> impl Future<Output = Self> + Send + 'static {
        self.send_reload(ReloadedProps::All)
    }

    /// Reload only the given props with a partial reload, then assert that the
    /// reloaded page has them, and the same component, URL and asset version.
    /// Await it for the reloaded page.
    ///
    /// ```no_run
    /// # async fn test(page: inertia::testing::AssertablePage) {
    /// page.reload_only(&["reviews"]).await.has_count("reviews", 3).missing("post");
    /// # }
    /// ```
    ///
    /// # Panics
    ///
    /// Panics when the page wasn't read by an adapter that can
    /// [send reloads](Self::reloads_with). The future panics when the
    /// reloaded page differs or misses one of the props.
    #[track_caller]
    pub fn reload_only(&self, props: &[&str]) -> impl Future<Output = Self> + Send + 'static {
        self.send_reload(ReloadedProps::Only(props.iter().map(|&prop| prop.to_owned()).collect()))
    }

    /// Reload every prop except the given ones with a partial reload, then
    /// assert that the reloaded page doesn't have them, and has the same
    /// component, URL and asset version. Await it for the reloaded page.
    ///
    /// ```no_run
    /// # async fn test(page: inertia::testing::AssertablePage) {
    /// page.reload_except(&["head"]).await.has("post");
    /// # }
    /// ```
    ///
    /// # Panics
    ///
    /// Panics when the page wasn't read by an adapter that can
    /// [send reloads](Self::reloads_with). The future panics when the
    /// reloaded page differs or has one of the props.
    #[track_caller]
    pub fn reload_except(&self, props: &[&str]) -> impl Future<Output = Self> + Send + 'static {
        self.send_reload(ReloadedProps::Except(
            props.iter().map(|&prop| prop.to_owned()).collect(),
        ))
    }

    #[track_caller]
    fn send_reload(&self, props: ReloadedProps) -> impl Future<Output = Self> + Send + 'static {
        let Some(reloader) = self.reloader.clone() else {
            panic!(
                "this Inertia page can't reload, because it wasn't read from a response that knows how to send requests"
            )
        };
        let request = ReloadRequest::new(&self.page, &props, Location::caller());
        let original = self.page.clone();

        async move {
            let location = request.location;
            let reloaded = reloader(request).await;
            reloaded.assert_reload_of(&original, &props, location);
            reloaded
        }
    }

    fn assert_reload_of(&self, original: &Page, props: &ReloadedProps, location: &Location<'_>) {
        for (field, expected, actual) in [
            ("page component", &original.component, &self.page.component),
            ("page URL", &original.url, &self.page.url),
            ("asset version", &original.version, &self.page.version),
        ] {
            assert!(
                actual == expected,
                "expected the reloaded Inertia {field} `{expected}`, but it's `{actual}`\n\n{}\n\nat {location}",
                self.props()
            );
        }

        match props {
            ReloadedProps::All => {}
            ReloadedProps::Only(props) => {
                for prop in props {
                    assert!(
                        self.prop(prop).is_some(),
                        "expected the reloaded Inertia page to have prop `{prop}`\n\n{}\n\nat {location}",
                        self.props()
                    );
                }
            }
            ReloadedProps::Except(props) => {
                for prop in props {
                    assert!(
                        self.prop(prop).is_none(),
                        "expected the reloaded Inertia page not to have prop `{prop}`\n\n{}\n\nat {location}",
                        self.props()
                    );
                }
            }
        }
    }
}

impl fmt::Debug for AssertablePage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AssertablePage")
            .field("page", &self.page)
            .finish_non_exhaustive()
    }
}

/// The props a reload asks for.
enum ReloadedProps {
    All,
    Only(Vec<String>),
    Except(Vec<String>),
}

/// The Inertia visit that reloads a page under test, for an adapter to send to
/// the application. See [`AssertablePage::reloads_with`].
#[derive(Debug, Clone)]
pub struct ReloadRequest {
    url: String,
    headers: HeaderMap,
    location: &'static Location<'static>,
}

impl ReloadRequest {
    fn new(page: &Page, props: &ReloadedProps, location: &'static Location<'static>) -> Self {
        let mut headers = HeaderMap::new();
        headers.insert(header::INERTIA, HeaderValue::from_static("true"));
        headers.insert(header::VERSION, header_value(&page.version));

        let partial = match props {
            ReloadedProps::All => None,
            ReloadedProps::Only(props) => Some((header::PARTIAL_ONLY, props)),
            ReloadedProps::Except(props) => Some((header::PARTIAL_EXCEPT, props)),
        };
        if let Some((name, props)) = partial.filter(|(_, props)| !props.is_empty()) {
            headers.insert(header::PARTIAL_COMPONENT, header_value(&page.component));
            headers.insert(name, header_value(&props.join(",")));
        }

        Self {
            url: page.url.clone(),
            headers,
            location,
        }
    }

    /// The URL to send a `GET` request to: the page's URL, such as `/users?page=2`.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The Inertia headers to send: `X-Inertia`, the page's asset version, and
    /// for a partial reload, the page's component and the props to include or
    /// exclude.
    pub const fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// Where the test reloaded the page, for an adapter's failure messages.
    pub const fn location(&self) -> &'static Location<'static> {
        self.location
    }
}

fn header_value(value: &str) -> HeaderValue {
    HeaderValue::try_from(value).unwrap_or_else(|error| panic!("`{value}` can't be sent in an Inertia header: {error}"))
}

/// The contents of the page data `<script>` of an HTML document.
#[track_caller]
fn page_script(html: &str) -> &str {
    let Some((json, _)) = html
        .split_once(r#"type="application/json">"#)
        .and_then(|(_, rest)| rest.split_once("</script>"))
    else {
        // Keep the panic outside a closure so #[track_caller] reaches the test...
        panic!("the HTML document doesn't contain Inertia page data\n\n{html}")
    };
    json
}

#[cfg(test)]
mod tests {
    use super::*;

    const HTML: &str = r#"<html><body><script data-page="app" type="application/json">{"component":"Home","props":{"user":{"name":"Taylor","roles":["admin"]}},"url":"\/","version":"1"}</script><div id="app"></div></body></html>"#;

    #[test]
    fn reads_pages_from_html_documents() {
        AssertablePage::from_body(HTML)
            .component("Home")
            .url("/")
            .has("user.name")
            .equals("user.roles.0", "admin")
            .has_count("user.roles", 1)
            .missing("user.email");
    }

    fn failure(assertion: impl FnOnce(&AssertablePage)) -> String {
        let page = AssertablePage::from_body(
            r#"{"component":"Error","props":{"status":404,"message":"Not Found"},"url":"/users/1","version":"1"}"#,
        );
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| assertion(&page)))
            .expect_err("the assertion should fail");

        panic.downcast_ref::<String>().cloned().unwrap_or_default()
    }

    #[test]
    fn page_failures_show_the_props_that_explain_them() {
        let props = "{\n  \"status\": 404,\n  \"message\": \"Not Found\"\n}";

        assert_eq!(
            failure(|page| {
                page.component("Users/Show");
            }),
            format!("expected the Inertia page component `Users/Show`, but it's `Error`\n\n{props}")
        );
        assert_eq!(
            failure(|page| {
                page.url("/users/2");
            }),
            format!("expected the Inertia page URL `/users/2`, but it's `/users/1`\n\n{props}")
        );
        assert_eq!(
            failure(|page| {
                page.version("2");
            }),
            format!("expected the Inertia asset version `2`, but it's `1`\n\n{props}")
        );
    }

    const USERS: &str =
        r#"{"component":"Users/Index","props":{"users":[],"filters":{}},"url":"/users?page=2","version":"abc"}"#;

    /// A page whose reloads respond with the given body, and the reloads it sent.
    fn reloading_to(body: &'static str) -> (AssertablePage, Arc<std::sync::Mutex<Vec<ReloadRequest>>>) {
        let sent: Arc<std::sync::Mutex<Vec<ReloadRequest>>> = Arc::default();
        let page = AssertablePage::from_body(USERS).reloads_with({
            let sent = Arc::clone(&sent);
            move |reload| {
                sent.lock().unwrap().push(reload);
                async move { AssertablePage::from_body(body) }
            }
        });

        (page, sent)
    }

    fn headers(reload: &ReloadRequest) -> Vec<(&str, &str)> {
        let mut headers: Vec<_> = reload
            .headers()
            .iter()
            .map(|(name, value)| (name.as_str(), value.to_str().unwrap()))
            .collect();
        headers.sort_unstable();
        headers
    }

    #[tokio::test]
    async fn reloads_are_inertia_visits_to_the_page() {
        let (page, sent) = reloading_to(USERS);

        page.reload().await;
        page.reload_only(&["users", "filters"]).await;
        page.reload_except(&["filters.sort"]).await;

        let sent = sent.lock().unwrap();
        assert!(sent.iter().all(|reload| reload.url() == "/users?page=2"));
        assert_eq!(headers(&sent[0]), [("x-inertia", "true"), ("x-inertia-version", "abc")]);
        assert_eq!(
            headers(&sent[1]),
            [
                ("x-inertia", "true"),
                ("x-inertia-partial-component", "Users/Index"),
                ("x-inertia-partial-data", "users,filters"),
                ("x-inertia-version", "abc"),
            ]
        );
        assert_eq!(
            headers(&sent[2]),
            [
                ("x-inertia", "true"),
                ("x-inertia-partial-component", "Users/Index"),
                ("x-inertia-partial-except", "filters.sort"),
                ("x-inertia-version", "abc"),
            ]
        );
    }

    /// The panic message of a reload, and the line it names.
    async fn reload_failure(reload: impl Future<Output = AssertablePage> + Send + 'static) -> (String, u32) {
        let panic = tokio::spawn(reload)
            .await
            .expect_err("the reload should fail")
            .into_panic();
        let message = panic.downcast_ref::<String>().cloned().unwrap_or_default();
        let (message, location) = message.rsplit_once("\n\nat ").expect("the failure names its location");
        let line = location
            .strip_prefix(concat!(file!(), ":"))
            .expect("the location is in the test");

        (message.to_owned(), line.split(':').next().unwrap().parse().unwrap())
    }

    #[tokio::test]
    async fn reloads_fail_when_the_reloaded_page_differs() {
        let (page, _) =
            reloading_to(r#"{"component":"Error","props":{"status":409},"url":"/users?page=2","version":"abc"}"#);
        let line = line!() + 1;
        let (message, at) = reload_failure(page.reload()).await;

        assert_eq!(
            message,
            "expected the reloaded Inertia page component `Users/Index`, but it's `Error`\n\n{\n  \"status\": 409\n}"
        );
        assert_eq!(at, line);

        let (page, _) =
            reloading_to(r#"{"component":"Users/Index","props":{"users":[]},"url":"/users?page=2","version":"abc"}"#);
        let (message, _) = reload_failure(page.reload_only(&["users", "filters"])).await;
        assert_eq!(
            message,
            "expected the reloaded Inertia page to have prop `filters`\n\n{\n  \"users\": []\n}"
        );

        let (message, _) = reload_failure(page.reload_except(&["users"])).await;
        assert_eq!(
            message,
            "expected the reloaded Inertia page not to have prop `users`\n\n{\n  \"users\": []\n}"
        );
    }
}
