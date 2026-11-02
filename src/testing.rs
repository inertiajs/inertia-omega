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

use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::ops::{Bound, RangeBounds};
use std::panic::Location;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use http::{HeaderMap, HeaderValue};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::header;
use crate::json::decode_big_integers;
use crate::page::Page;

/// A page under test. Each assertion panics with a descriptive message when it fails.
///
/// Prop paths use dot notation, such as `users.0.name`. Inside a
/// [`scope`](Self::scope), they start at the scoped prop.
#[derive(Clone)]
pub struct AssertablePage {
    page: Arc<Page>,
    reloader: Option<Reloader>,
    pages: Option<Arc<Path>>,
    scope: Option<Arc<Scope>>,
}

type Reloader = Arc<dyn Fn(ReloadRequest) -> Pin<Box<dyn Future<Output = AssertablePage> + Send>> + Send + Sync>;

/// The prop that a [scoped](AssertablePage::scope) closure asserts on, and the
/// keys of it that the closure has looked at.
struct Scope {
    path: String,
    interacted: Mutex<BTreeSet<String>>,
    etc: AtomicBool,
}

/// The file extensions of page components.
const PAGE_EXTENSIONS: [&str; 6] = ["js", "jsx", "svelte", "ts", "tsx", "vue"];

impl AssertablePage {
    /// Read the page from a response body: the JSON of an Inertia visit, or
    /// the HTML document of a first visit. Big integers that the page
    /// [preserves](Page::preserve_big_integers) read as numbers again.
    #[track_caller]
    pub fn from_body(body: &str) -> Self {
        let json = if body.trim_start().starts_with('{') {
            body
        } else {
            page_script(body)
        };

        let mut page: Page = match serde_json::from_str(json) {
            Ok(page) => page,
            Err(error) => panic!("the response body doesn't contain a valid Inertia page: {error}\n\n{body}"),
        };
        if page.preserve_big_integers {
            page.props = decode_map(std::mem::take(&mut page.props));
            page.flash = decode_map(std::mem::take(&mut page.flash));
        }

        Self {
            page: Arc::new(page),
            reloader: None,
            pages: None,
            scope: None,
        }
    }

    /// Check that each [`component`](Self::component) the test asserts has a
    /// page file in the given directory, such as `Users/Show.tsx` for
    /// `Users/Show`, with the extension `js`, `jsx`, `svelte`, `ts`, `tsx`
    /// or `vue`. Adapters call this with the application's pages directory.
    #[must_use]
    pub fn pages_in(mut self, directory: impl Into<PathBuf>) -> Self {
        self.pages = Some(Arc::from(directory.into()));
        self
    }

    /// The page.
    pub fn page(&self) -> &Page {
        &self.page
    }

    /// The prop at the given path. Inside a [`scope`](Self::scope), reading a
    /// prop counts as asserting on it.
    pub fn prop(&self, path: &str) -> Option<&Value> {
        if let Some(scope) = &self.scope {
            let key = path.split('.').next().unwrap_or(path);
            scope
                .interacted
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(key.to_owned());
        }

        descend(&self.page.props, &self.full_path(path))
    }

    /// The prop at the given path, or a failure naming it.
    #[track_caller]
    fn expect_prop(&self, path: &str) -> &Value {
        match self.prop(path) {
            Some(value) => value,
            None => panic!(
                "expected the Inertia page to have prop `{}`\n\n{}",
                self.full_path(path),
                self.props()
            ),
        }
    }

    /// The path from the page's props, for a path inside this scope.
    fn full_path(&self, path: &str) -> String {
        match &self.scope {
            Some(scope) => format!("{}.{path}", scope.path),
            None => path.to_owned(),
        }
    }

    /// Assert the page component. A failure shows the page's props, such as
    /// the `status` and `message` of an error page rendered instead. When the
    /// page knows its [pages directory](Self::pages_in), also assert that the
    /// component has a page file.
    #[track_caller]
    pub fn component(&self, expected: &str) -> &Self {
        self.assert_field("page component", &self.page.component, expected);

        if let Some(pages) = &self.pages {
            let exists = PAGE_EXTENSIONS
                .iter()
                .any(|extension| pages.join(format!("{expected}.{extension}")).is_file());
            assert!(
                exists,
                "the Inertia page component `{expected}` has no page file in {}, such as `{expected}.tsx` or `{expected}.vue`",
                pages.display()
            );
        }
        self
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
        self.expect_prop(path);
        self
    }

    /// Assert that at least one of the props exists.
    #[track_caller]
    pub fn has_any(&self, paths: &[&str]) -> &Self {
        let found: Vec<bool> = paths.iter().map(|path| self.prop(path).is_some()).collect();
        assert!(
            found.contains(&true),
            "expected the Inertia page to have one of props {}\n\n{}",
            paths
                .iter()
                .map(|path| format!("`{}`", self.full_path(path)))
                .collect::<Vec<_>>()
                .join(", "),
            self.props()
        );
        self
    }

    /// Assert that the prop is an array or object with the given number of items.
    #[track_caller]
    pub fn has_count(&self, path: &str, count: usize) -> &Self {
        self.has_count_in(path, count..=count)
    }

    /// Assert that the prop is an array or object whose number of items is in
    /// the range, such as `1..=3`.
    #[track_caller]
    pub fn has_count_in(&self, path: &str, counts: impl RangeBounds<usize> + fmt::Debug) -> &Self {
        let actual = self.items(path).len();
        assert!(
            counts.contains(&actual),
            "expected Inertia prop `{}` to have {} items, but it has {actual}",
            self.full_path(path),
            match (counts.start_bound(), counts.end_bound()) {
                (Bound::Included(start), Bound::Included(end)) if start == end => start.to_string(),
                _ => format!("{counts:?}"),
            }
        );
        self
    }

    /// The keys of the array or object at the path: its indexes, or its keys.
    #[track_caller]
    fn items(&self, path: &str) -> Vec<String> {
        match self.expect_prop(path) {
            Value::Array(items) => (0..items.len()).map(|index| index.to_string()).collect(),
            Value::Object(object) => object.keys().cloned().collect(),
            other => panic!(
                "expected Inertia prop `{}` to be an array or object, but it's {other}",
                self.full_path(path)
            ),
        }
    }

    /// Assert that the prop doesn't exist.
    #[track_caller]
    pub fn missing(&self, path: &str) -> &Self {
        assert!(
            self.prop(path).is_none(),
            "expected the Inertia page not to have prop `{}`\n\n{}",
            self.full_path(path),
            self.props()
        );
        self
    }

    /// Assert that the prop equals the given value. Objects match in any key order.
    #[track_caller]
    pub fn equals(&self, path: &str, expected: impl Serialize) -> &Self {
        let expected = serde_json::to_value(expected).expect("the expected value should serialize");
        let actual = self.expect_prop(path);

        assert_eq!(
            actual,
            &expected,
            "unexpected value of Inertia prop `{}`",
            self.full_path(path)
        );
        self
    }

    /// Assert that the prop reads as a `T` that passes the check. Name the
    /// type in the closure, as in `|views: u64| views > 10`. To assert only
    /// the type, return `true`.
    ///
    /// ```
    /// # let page = inertia::testing::AssertablePage::from_body(r#"{"component":"Posts/Show","props":{"post":{"views":12,"published_at":"2026-01-01"}},"url":"/","version":"1"}"#);
    /// page.satisfies("post.views", |views: u64| views > 10)
    ///     .satisfies("post.published_at", |at: Option<String>| at.is_some());
    /// ```
    #[track_caller]
    pub fn satisfies<T: DeserializeOwned>(&self, path: &str, check: impl FnOnce(T) -> bool) -> &Self {
        let actual = self.expect_prop(path);
        let value = serde_json::from_value(actual.clone()).unwrap_or_else(|error| {
            panic!(
                "expected Inertia prop `{}` to read as `{}`: {error}\n\n{actual}",
                self.full_path(path),
                std::any::type_name::<T>()
            )
        });

        assert!(
            check(value),
            "Inertia prop `{}` failed the check: {actual}",
            self.full_path(path)
        );
        self
    }

    /// Assert that the array or object prop has an item equal to the given value.
    #[track_caller]
    pub fn contains(&self, path: &str, item: impl Serialize) -> &Self {
        let item = serde_json::to_value(item).expect("the expected value should serialize");
        let found = match self.expect_prop(path) {
            Value::Array(items) => items.contains(&item),
            Value::Object(object) => object.values().any(|value| value == &item),
            other => panic!(
                "expected Inertia prop `{}` to be an array or object, but it's {other}",
                self.full_path(path)
            ),
        };

        assert!(
            found,
            "expected Inertia prop `{}` to contain {item}\n\n{}",
            self.full_path(path),
            serde_json::to_string_pretty(self.expect_prop(path)).unwrap_or_default()
        );
        self
    }

    /// Assert on the object or array at the path, with paths that start there.
    /// The closure must assert on every prop in the scope, or call
    /// [`etc`](Self::etc) to allow the others.
    ///
    /// ```
    /// # let page = inertia::testing::AssertablePage::from_body(r#"{"component":"Users/Show","props":{"user":{"name":"Taylor","email":"taylor@example.com","team":{"name":"Laravel"}}},"url":"/","version":"1"}"#);
    /// page.scope("user", |user| {
    ///     user.equals("name", "Taylor")
    ///         .has("email")
    ///         .scope("team", |team| team.equals("name", "Laravel"))
    /// });
    /// ```
    ///
    /// Scoped assertions such as [`component`](Self::component) and
    /// [`reload`](Self::reload) still apply to the whole page.
    #[track_caller]
    pub fn scope(&self, path: &str, assertions: impl for<'a> FnOnce(&'a Self) -> &'a Self) -> &Self {
        self.assert_scoped(path, assertions);
        self
    }

    /// Assert on the first item of the array or object at the path, as
    /// [`scope`](Self::scope) does.
    ///
    /// ```
    /// # let page = inertia::testing::AssertablePage::from_body(r#"{"component":"Users/Index","props":{"users":[{"id":1,"name":"Taylor"}]},"url":"/","version":"1"}"#);
    /// page.has_count("users", 1)
    ///     .first("users", |user| user.equals("id", 1).etc());
    /// ```
    #[track_caller]
    pub fn first(&self, path: &str, assertions: impl for<'a> FnOnce(&'a Self) -> &'a Self) -> &Self {
        let Some(first) = self.items(path).into_iter().next() else {
            panic!("expected Inertia prop `{}` to have an item", self.full_path(path))
        };

        self.assert_scoped(&format!("{path}.{first}"), assertions);
        self
    }

    /// Assert on each item of the array or object at the path, as
    /// [`scope`](Self::scope) does.
    ///
    /// ```
    /// # let page = inertia::testing::AssertablePage::from_body(r#"{"component":"Users/Index","props":{"users":[{"id":1,"name":"Taylor"}]},"url":"/","version":"1"}"#);
    /// page.each("users", |user| user.has("id").missing("password").etc());
    /// ```
    #[track_caller]
    pub fn each(&self, path: &str, mut assertions: impl for<'a> FnMut(&'a Self) -> &'a Self) -> &Self {
        let items = self.items(path);
        assert!(
            !items.is_empty(),
            "expected Inertia prop `{}` to have an item",
            self.full_path(path)
        );

        for item in items {
            self.assert_scoped(&format!("{path}.{item}"), &mut assertions);
        }
        self
    }

    /// Allow the props in this [`scope`](Self::scope) that the closure doesn't
    /// assert on.
    pub fn etc(&self) -> &Self {
        if let Some(scope) = &self.scope {
            scope.etc.store(true, Ordering::Relaxed);
        }
        self
    }

    #[track_caller]
    fn assert_scoped(&self, path: &str, assertions: impl for<'a> FnOnce(&'a Self) -> &'a Self) {
        let keys = self.items(path);
        let scoped = Self {
            scope: Some(Arc::new(Scope {
                path: self.full_path(path),
                interacted: Mutex::default(),
                etc: AtomicBool::new(false),
            })),
            ..self.clone()
        };

        assertions(&scoped);

        let scope = scoped.scope.as_ref().expect("the page was just scoped");
        if scope.etc.load(Ordering::Relaxed) {
            return;
        }
        let interacted = scope.interacted.lock().unwrap_or_else(PoisonError::into_inner);
        let unexpected: Vec<String> = keys
            .iter()
            .filter(|key| !interacted.contains(*key))
            .map(|key| format!("`{key}`"))
            .collect();
        assert!(
            unexpected.is_empty(),
            "unexpected props in Inertia prop `{}`: {}. Assert on them, or call `etc()` to allow them\n\n{}",
            scope.path,
            unexpected.join(", "),
            scoped.props()
        );
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

    /// Assert the page's flash data at the given path.
    #[track_caller]
    pub fn flash(&self, path: &str, expected: impl Serialize) -> &Self {
        let expected = serde_json::to_value(expected).expect("the expected value should serialize");

        assert_eq!(
            descend(&self.page.flash, path),
            Some(&expected),
            "unexpected Inertia flash data `{path}`"
        );
        self
    }

    /// Assert that the page's flash data has the given path.
    #[track_caller]
    pub fn has_flash(&self, path: &str) -> &Self {
        assert!(
            descend(&self.page.flash, path).is_some(),
            "expected the Inertia page to have flash data `{path}`\n\n{}",
            self.flash_data()
        );
        self
    }

    /// Assert that the page's flash data doesn't have the given path.
    #[track_caller]
    pub fn missing_flash(&self, path: &str) -> &Self {
        assert!(
            descend(&self.page.flash, path).is_none(),
            "expected the Inertia page not to have flash data `{path}`\n\n{}",
            self.flash_data()
        );
        self
    }

    /// The props in this scope, for failure messages.
    fn props(&self) -> String {
        let props = match &self.scope {
            Some(scope) => descend(&self.page.props, &scope.path).cloned().unwrap_or_default(),
            None => Value::Object(self.page.props.clone()),
        };
        serde_json::to_string_pretty(&props).unwrap_or_default()
    }

    fn flash_data(&self) -> String {
        serde_json::to_string_pretty(&self.page.flash).unwrap_or_default()
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
    pub fn reload(&self) -> impl Future<Output = Self> + Send + use<> {
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
    pub fn reload_only(&self, props: &[&str]) -> impl Future<Output = Self> + Send + use<> {
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
    pub fn reload_except(&self, props: &[&str]) -> impl Future<Output = Self> + Send + use<> {
        self.send_reload(ReloadedProps::Except(
            props.iter().map(|&prop| prop.to_owned()).collect(),
        ))
    }

    /// Load every deferred prop with a partial reload, as the Inertia client
    /// does after the first render, then assert that the reloaded page has
    /// them. Await it for the reloaded page.
    ///
    /// ```no_run
    /// # async fn test(page: inertia::testing::AssertablePage) {
    /// page.missing("stats")
    ///     .load_deferred_props()
    ///     .await
    ///     .equals("stats.visits", 42);
    /// # }
    /// ```
    ///
    /// # Panics
    ///
    /// Panics as [`reload_only`](Self::reload_only) does.
    #[track_caller]
    pub fn load_deferred_props(&self) -> impl Future<Output = Self> + Send + use<> {
        let groups: Vec<&str> = self.page.metadata.deferred_props.keys().map(String::as_str).collect();
        self.load_deferred_groups(&groups)
    }

    /// Load the deferred props of the given groups with a partial reload, as
    /// [`load_deferred_props`](Self::load_deferred_props) does for all of them.
    ///
    /// # Panics
    ///
    /// Panics as [`reload_only`](Self::reload_only) does.
    #[track_caller]
    pub fn load_deferred_groups(&self, groups: &[&str]) -> impl Future<Output = Self> + Send + use<> {
        let props = groups
            .iter()
            .filter_map(|group| self.page.metadata.deferred_props.get(*group))
            .flatten()
            .cloned()
            .collect();
        self.send_reload(ReloadedProps::Only(props))
    }

    #[track_caller]
    fn send_reload(&self, props: ReloadedProps) -> impl Future<Output = Self> + Send + use<> {
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

/// The value at a dot-notation path, such as `users.0.name`.
fn descend<'a>(map: &'a Map<String, Value>, path: &str) -> Option<&'a Value> {
    let mut segments = path.split('.');
    let mut value = map.get(segments.next()?)?;

    for segment in segments {
        value = match value {
            Value::Object(object) => object.get(segment)?,
            Value::Array(items) => items.get(segment.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }

    Some(value)
}

fn decode_map(map: Map<String, Value>) -> Map<String, Value> {
    map.into_iter()
        .map(|(key, value)| (key, decode_big_integers(value)))
        .collect()
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

    const POSTS: &str = r#"{"component":"Posts/Index","props":{"posts":[{"id":1,"title":"Hello","author":{"name":"Taylor","email":"taylor@example.com"}},{"id":2,"title":"World","author":{"name":"Abigail","email":"abigail@example.com"}}],"tags":["rust","laravel"],"total":2},"url":"/posts","version":"1","flash":{"toast":{"message":"Saved!"}}}"#;

    /// The message of the assertion's failure on the posts page.
    fn posts_failure(assertion: impl FnOnce(&AssertablePage)) -> String {
        let page = AssertablePage::from_body(POSTS);
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| assertion(&page)))
            .expect_err("the assertion should fail");

        panic.downcast_ref::<String>().cloned().unwrap_or_default()
    }

    #[test]
    fn scopes_assert_on_every_prop_they_contain() {
        AssertablePage::from_body(POSTS)
            .first("posts", |post| {
                post.equals("id", 1)
                    .has("title")
                    .scope("author", |author| author.equals("name", "Taylor").has("email"))
            })
            .each("posts", |post| {
                post.has("author.email").missing("author.password").etc()
            })
            .scope("tags", |tags| tags.equals("0", "rust").etc());

        let message = posts_failure(|page| {
            page.first("posts", |post| {
                post.equals("id", 1).scope("author", |author| author.has("name").etc())
            });
        });
        assert_eq!(
            message.split_once("\n\n").unwrap().0,
            "unexpected props in Inertia prop `posts.0`: `title`. Assert on them, or call `etc()` to allow them"
        );

        let message = posts_failure(|page| {
            page.each("posts", |post| post.equals("author.name", "Taylor").etc());
        });
        assert!(
            message.contains("unexpected value of Inertia prop `posts.1.author.name`"),
            "{message}"
        );

        let message = posts_failure(|page| {
            page.scope("total", |total| total.etc());
        });
        assert_eq!(
            message,
            "expected Inertia prop `total` to be an array or object, but it's 2"
        );
    }

    #[test]
    fn props_can_be_checked_by_type_and_closure() {
        AssertablePage::from_body(POSTS)
            .satisfies("total", |total: u64| total == 2)
            .satisfies("posts.0.author", |author: serde_json::Map<String, Value>| {
                author.len() == 2
            })
            .contains("tags", "rust")
            .contains("posts.1", "World")
            .has_any(&["missing", "tags"])
            .has_count_in("posts", 1..=2)
            .has_count_in("tags", 2..);

        assert!(
            posts_failure(|page| {
                page.satisfies("total", |total: String| !total.is_empty());
            })
            .starts_with("expected Inertia prop `total` to read as `alloc::string::String`: invalid type: integer `2`"),
        );
        assert_eq!(
            posts_failure(|page| {
                page.satisfies("total", |total: u64| total > 2);
            }),
            "Inertia prop `total` failed the check: 2"
        );
        assert_eq!(
            posts_failure(|page| {
                page.contains("tags", "php");
            }),
            "expected Inertia prop `tags` to contain \"php\"\n\n[\n  \"rust\",\n  \"laravel\"\n]"
        );
        assert!(
            posts_failure(|page| {
                page.has_any(&["users", "comments"]);
            })
            .starts_with("expected the Inertia page to have one of props `users`, `comments`"),
        );
        assert_eq!(
            posts_failure(|page| {
                page.has_count_in("posts", 3..=5);
            }),
            "expected Inertia prop `posts` to have 3..=5 items, but it has 2"
        );
        assert_eq!(
            posts_failure(|page| {
                page.has_count("tags", 3);
            }),
            "expected Inertia prop `tags` to have 3 items, but it has 2"
        );
    }

    #[test]
    fn flash_data_is_read_by_path() {
        AssertablePage::from_body(POSTS)
            .flash("toast.message", "Saved!")
            .has_flash("toast")
            .missing_flash("toast.level")
            .missing_flash("error");

        assert_eq!(
            posts_failure(|page| {
                page.has_flash("error");
            }),
            "expected the Inertia page to have flash data `error`\n\n{\n  \"toast\": {\n    \"message\": \"Saved!\"\n  }\n}"
        );
    }

    #[test]
    fn preserved_big_integers_read_as_numbers() {
        let body = r#"{"component":"Orders/Show","props":{"order":{"id":{"$bigint":"9007199254740993"}}},"url":"/","version":"1","preserveBigIntegers":true,"flash":{"refund":{"$bigint":"-9007199254740993"}}}"#;

        AssertablePage::from_body(body)
            .equals("order.id", 9_007_199_254_740_993_u64)
            .flash("refund", -9_007_199_254_740_993_i64);
    }

    #[test]
    fn components_must_have_a_page_file_in_the_pages_directory() {
        let pages = std::env::temp_dir().join(format!("inertia-pages-{}", std::process::id()));
        std::fs::create_dir_all(pages.join("Posts")).unwrap();
        std::fs::write(pages.join("Posts/Index.vue"), "").unwrap();

        AssertablePage::from_body(POSTS)
            .pages_in(&pages)
            .component("Posts/Index");
        let page = AssertablePage::from_body(&POSTS.replace("Posts/Index", "Posts/Indx")).pages_in(&pages);
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            page.component("Posts/Indx");
        }))
        .expect_err("the page file is missing");
        std::fs::remove_dir_all(&pages).unwrap();

        assert_eq!(
            panic.downcast_ref::<String>().unwrap(),
            &format!(
                "the Inertia page component `Posts/Indx` has no page file in {}, such as `Posts/Indx.tsx` or `Posts/Indx.vue`",
                pages.display()
            )
        );
    }

    #[tokio::test]
    async fn deferred_props_load_with_a_partial_reload() {
        let deferred = r#"{"component":"Users/Index","props":{"users":[],"filters":{}},"url":"/users?page=2","version":"abc","deferredProps":{"default":["stats"],"sidebar":["teams","tags"]}}"#;
        let loaded = r#"{"component":"Users/Index","props":{"stats":1,"teams":[],"tags":[]},"url":"/users?page=2","version":"abc"}"#;
        let sent: Arc<std::sync::Mutex<Vec<ReloadRequest>>> = Arc::default();
        let page = AssertablePage::from_body(deferred).reloads_with({
            let sent = Arc::clone(&sent);
            move |reload| {
                sent.lock().unwrap().push(reload);
                async move { AssertablePage::from_body(loaded) }
            }
        });

        page.load_deferred_props().await.equals("stats", 1);
        page.load_deferred_groups(&["sidebar"]).await.has("teams");

        let sent = sent.lock().unwrap();
        assert_eq!(sent[0].headers()["x-inertia-partial-data"], "stats,teams,tags");
        assert_eq!(sent[1].headers()["x-inertia-partial-data"], "teams,tags");
    }
}
