//! Page props: the data a page component receives, and the behavior that
//! controls when and how each prop is sent.

mod prop;
mod resolver;
mod scroll;

use std::fmt;
use std::future::Future;

use indexmap::IndexMap;
use serde::Serialize;
use serde_json::Value;

pub(crate) use self::prop::{Computed, Loading, Merge, Once, Source};
pub use self::prop::{IntoProp, Prop};
pub(crate) use self::resolver::PropsResolver;
pub use self::scroll::{Paginator, ProvidesScrollMetadata, ScrollMetadata};

/// A boxed error, as returned by fallible prop callbacks.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// An error raised while resolving a prop.
#[derive(Debug)]
pub struct PropError(BoxError);

impl PropError {
    /// Wrap an error.
    pub fn new(error: impl Into<BoxError>) -> Self {
        Self(error.into())
    }

    /// The error the prop failed with.
    pub fn into_inner(self) -> BoxError {
        self.0
    }
}

impl fmt::Display for PropError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for PropError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&*self.0)
    }
}

impl From<serde_json::Error> for PropError {
    fn from(error: serde_json::Error) -> Self {
        Self::new(error)
    }
}

/// An ordered set of page props.
///
/// Keys may use dot notation (`"auth.user"`) to set nested props. The
/// [`props!`](crate::props!) macro is the most convenient way to build one.
#[derive(Debug, Default)]
pub struct Props(pub(crate) IndexMap<String, Prop>);

impl Props {
    /// Create an empty set of props.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a prop, replacing any prop with the same key.
    pub fn insert(&mut self, key: impl Into<String>, value: impl IntoProp) -> &mut Self {
        self.0.insert(key.into(), value.into_prop());
        self
    }

    /// Insert a prop, returning the set for chaining.
    pub fn with(mut self, key: impl Into<String>, value: impl IntoProp) -> Self {
        self.insert(key, value);
        self
    }

    /// Remove a prop.
    pub fn remove(&mut self, key: &str) -> Option<Prop> {
        self.0.shift_remove(key)
    }

    /// Whether a prop with the given key exists.
    pub fn contains_key(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }

    /// The prop keys, in order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    /// The number of props.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are no props.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<K: Into<String>, V: IntoProp> Extend<(K, V)> for Props {
    fn extend<I: IntoIterator<Item = (K, V)>>(&mut self, iter: I) {
        for (key, value) in iter {
            self.insert(key, value);
        }
    }
}

impl<K: Into<String>, V: IntoProp> FromIterator<(K, V)> for Props {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut props = Self::new();
        props.extend(iter);
        props
    }
}

impl IntoIterator for Props {
    type Item = (String, Prop);
    type IntoIter = indexmap::map::IntoIter<String, Prop>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

/// Conversion into the props of a page.
///
/// Implemented for [`Props`] and for any `Serialize` type that serializes to
/// a JSON object (such as a struct) or to `null` (such as `()`).
pub trait IntoProps {
    /// Convert the value into props.
    ///
    /// # Errors
    ///
    /// Fails when the value doesn't serialize to a JSON object or `null`.
    fn into_props(self) -> Result<Props, PropError>;
}

impl IntoProps for Props {
    fn into_props(self) -> Result<Props, PropError> {
        Ok(self)
    }
}

impl<T: Serialize> IntoProps for T {
    fn into_props(self) -> Result<Props, PropError> {
        match serde_json::to_value(self)? {
            Value::Object(object) => Ok(object.into_iter().collect()),
            Value::Null => Ok(Props::new()),
            other => Err(PropError::new(format!(
                "page props must serialize to a JSON object, not `{other}`"
            ))),
        }
    }
}

/// Build a set of [`Props`]: from the variables it names, `name => value`
/// pairs, or `"name" => value` pairs for keys that aren't Rust identifiers,
/// such as dotted paths. A key is the name as written.
///
/// ```
/// let user = String::from("Taylor");
///
/// let props = inertia::props! {
///     user,
///     can_edit => true,
///     "auth.user.id" => 1,
///     stats => inertia::defer(async || 42),
/// };
///
/// assert_eq!(user, "Taylor");
/// ```
///
/// A variable named on its own is borrowed and serialized straight away, so
/// the caller keeps it. A `name => value` pair takes the value as written:
/// pass `&value` to keep it, and move a [`Prop`], such as a deferred one,
/// with `stats => stats`.
///
/// `if`, `else if`, `else` and `if let` blocks add their props only when
/// their condition holds:
///
/// ```
/// # let (is_admin, team) = (true, Some("Laravel"));
/// let props = inertia::props! {
///     if is_admin {
///         stats => inertia::defer(async || 42),
///     } else {
///         upgrade => "Upgrade to see stats",
///     }
///     if let Some(team) = team {
///         team,
///     }
/// };
///
/// assert!(props.contains_key("stats") && props.contains_key("team"));
/// assert!(!props.contains_key("upgrade"));
/// ```
#[macro_export]
macro_rules! props {
    ($($tokens:tt)*) => {
        $crate::__private::props!($crate; $($tokens)*)
    };
}

/// What a lazy prop's callback returns: a value to serialize, or a
/// `Result` of one, whose error fails the response unless the prop is
/// [rescued](Prop::rescue).
///
/// The marker `M` tells the two apart. A callback that returns a `Result`
/// whose `Ok` and `Err` types are both `Serialize`, such as
/// `Result<T, String>`, matches both; use [`try_lazy`] for it.
#[diagnostic::on_unimplemented(
    message = "a lazy prop's callback returns `{Self}`, which isn't a prop value",
    label = "this callback",
    note = "return a `Serialize` value, or a `Result` of one whose error converts into `Box<dyn Error + Send + Sync>`"
)]
pub trait Loaded<M> {
    #[doc(hidden)]
    fn into_json(self) -> Result<Value, PropError>;
}

#[doc(hidden)]
pub enum Plain {}

#[doc(hidden)]
pub enum Fallible {}

impl<T: Serialize> Loaded<Plain> for T {
    fn into_json(self) -> Result<Value, PropError> {
        Ok(serde_json::to_value(self)?)
    }
}

impl<T: Serialize, E: Into<BoxError>> Loaded<Fallible> for Result<T, E> {
    fn into_json(self) -> Result<Value, PropError> {
        Ok(serde_json::to_value(self.map_err(PropError::new)?)?)
    }
}

/// A prop computed by a callback, which only runs when the prop is part of
/// the response, after the handler has returned. The callback owns what it
/// uses, so move values into it.
///
/// The callback may return a `Result`. Its error fails the response, unless
/// the prop is [rescued](Prop::rescue).
///
/// ```
/// # async fn load_stats() -> Result<u32, std::io::Error> { Ok(1) }
/// let user = inertia::lazy(async || "Taylor");
/// let stats = inertia::lazy(async || load_stats().await);
/// ```
pub fn lazy<F, Fut, T, M>(callback: F) -> Prop
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = T> + Send + 'static,
    T: Loaded<M>,
{
    Prop::from_source(Source::Callback(Box::new(move || {
        Box::pin(async move {
            Ok(Computed {
                value: callback().await.into_json()?,
                scroll: None,
            })
        })
    })))
}

/// A prop computed by a fallible callback. An error fails the response,
/// unless the prop is [rescued](Prop::rescue).
///
/// ```
/// # async fn load_stats() -> Result<u32, std::io::Error> { Ok(1) }
/// let stats = inertia::try_lazy(|| load_stats()).deferred().rescue();
/// ```
pub fn try_lazy<F, Fut, T, E>(callback: F) -> Prop
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = Result<T, E>> + Send + 'static,
    T: Serialize,
    E: Into<BoxError>,
{
    Prop::from_source(Source::Callback(Box::new(move || {
        Box::pin(async move {
            let value = callback().await.map_err(PropError::new)?;

            Ok(Computed {
                value: serde_json::to_value(value)?,
                scroll: None,
            })
        })
    })))
}

/// A prop that is only sent when a partial reload asks for it.
pub fn optional<F, Fut, T, M>(callback: F) -> Prop
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = T> + Send + 'static,
    T: Loaded<M>,
{
    lazy(callback).optional()
}

/// A prop the client fetches right after the first render, so a slow prop
/// doesn't hold up the page.
pub fn defer<F, Fut, T, M>(callback: F) -> Prop
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = T> + Send + 'static,
    T: Loaded<M>,
{
    lazy(callback).deferred()
}

/// A prop that is resolved once and remembered by the client on later visits.
pub fn once<F, Fut, T, M>(callback: F) -> Prop
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = T> + Send + 'static,
    T: Loaded<M>,
{
    lazy(callback).once()
}

/// A prop that is sent with every response, even partial reloads that
/// didn't ask for it.
pub fn always(value: impl IntoProp) -> Prop {
    value.into_prop().always()
}

/// A prop that is appended to its client-side value instead of replacing it.
pub fn merge(value: impl IntoProp) -> Prop {
    value.into_prop().merge()
}

/// A prop that is deep merged with its client-side value.
pub fn deep_merge(value: impl IntoProp) -> Prop {
    value.into_prop().deep_merge()
}

/// An infinite scroll prop, from a page of items such as a [`Paginator`].
pub fn scroll<P>(page: P) -> Prop
where
    P: ProvidesScrollMetadata + Serialize,
{
    let metadata = page.scroll_metadata();

    scrollable(Prop::value(page), Some(metadata))
}

/// An infinite scroll prop, from a callback that loads a page of items.
pub fn scroll_with<F, Fut, P>(callback: F) -> Prop
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = P> + Send + 'static,
    P: ProvidesScrollMetadata + Serialize,
{
    let source = Source::Callback(Box::new(move || {
        Box::pin(async move {
            let page = callback().await;

            Ok(Computed {
                scroll: Some(page.scroll_metadata()),
                value: serde_json::to_value(page)?,
            })
        })
    }));

    scrollable(Prop::from_source(source), None)
}

fn scrollable(prop: Prop, metadata: Option<ScrollMetadata>) -> Prop {
    let mut prop = prop.merge();
    prop.options.scroll = Some(prop::Scroll {
        wrapper: "data".to_owned(),
        metadata,
    });
    prop
}
