use std::fmt;
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde::Serialize;
use serde_json::Value;

use super::{PropError, Props, ScrollMetadata};

/// A single page prop.
///
/// Any `Serialize` value converts into a prop; the functions at the crate
/// root ([`lazy`](crate::lazy), [`defer`](crate::defer), [`merge`](crate::merge), ...)
/// create props with special behavior, which the builder methods refine:
///
/// ```
/// let users = inertia::defer(|| async { vec!["Taylor", "Jess"] })
///     .group("sidebar")
///     .merge()
///     .match_on("id");
/// ```
#[derive(Debug)]
#[must_use]
pub struct Prop {
    pub(crate) source: Source,
    pub(crate) options: Options,
}

/// Where a prop's value comes from.
pub(crate) enum Source {
    /// An already serialized value.
    Value(Value),
    /// Nested props, which may have behavior of their own.
    Props(Props),
    /// A callback that only runs when the prop is part of the response.
    Callback(Callback),
    /// A callback whose output gains the props set inside it with dot
    /// notation (`"auth.user"` beside `"auth"`), by their remaining path,
    /// once it has run.
    Extended(Callback, Vec<(Vec<String>, Prop)>),
    /// A value that failed to serialize.
    Failed(PropError),
}

type Callback = Box<dyn FnOnce() -> BoxFuture<'static, Result<Computed, PropError>> + Send>;

/// The output of a prop callback.
pub(crate) struct Computed {
    pub(crate) value: Value,
    pub(crate) scroll: Option<ScrollMetadata>,
}

/// How a prop behaves during resolution.
#[derive(Debug, Clone, Default)]
pub(crate) struct Options {
    pub(crate) loading: Loading,
    pub(crate) always: bool,
    pub(crate) rescue: bool,
    pub(crate) merge: Option<Merge>,
    pub(crate) once: Option<Once>,
    pub(crate) scroll: Option<Scroll>,
}

/// When a prop is sent to the client.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum Loading {
    /// With every response that includes it.
    #[default]
    Eager,
    /// Only when a partial reload asks for it.
    Optional,
    /// Fetched by the client right after the first render, with its group.
    Deferred { group: String },
}

/// How a mergeable prop combines with its client-side value.
#[derive(Debug, Clone)]
pub(crate) struct Merge {
    pub(crate) deep: bool,
    pub(crate) append: bool,
    pub(crate) appends_at: Vec<String>,
    pub(crate) prepends_at: Vec<String>,
    pub(crate) match_on: Vec<String>,
}

impl Default for Merge {
    fn default() -> Self {
        Self {
            deep: false,
            append: true,
            appends_at: Vec::new(),
            prepends_at: Vec::new(),
            match_on: Vec::new(),
        }
    }
}

impl Merge {
    fn merges_at_root(&self) -> bool {
        self.appends_at.is_empty() && self.prepends_at.is_empty()
    }

    pub(crate) fn appends_at_root(&self) -> bool {
        self.append && self.merges_at_root()
    }

    pub(crate) fn prepends_at_root(&self) -> bool {
        !self.append && self.merges_at_root()
    }
}

/// The options of a prop the client remembers across visits.
#[derive(Debug, Clone, Default)]
pub(crate) struct Once {
    pub(crate) key: Option<String>,
    pub(crate) ttl: Option<Duration>,
    pub(crate) fresh: bool,
}

/// The options of an infinite scroll prop.
#[derive(Debug, Clone)]
pub(crate) struct Scroll {
    pub(crate) wrapper: String,
    pub(crate) metadata: Option<ScrollMetadata>,
}

impl Prop {
    pub(crate) fn from_source(source: Source) -> Self {
        Self {
            source,
            options: Options::default(),
        }
    }

    /// Create a prop from a serializable value.
    pub fn value(value: impl Serialize) -> Self {
        Self::from_source(match serde_json::to_value(value) {
            Ok(value) => Source::Value(value),
            Err(error) => Source::Failed(error.into()),
        })
    }

    /// Only send the prop when a partial reload asks for it.
    pub fn optional(mut self) -> Self {
        self.options.loading = Loading::Optional;
        self
    }

    /// Leave the prop out of the first render; the client fetches it right
    /// after, in the `default` group.
    pub fn deferred(self) -> Self {
        self.group("default")
    }

    /// Defer the prop, fetching it with the other props in the given group.
    pub fn group(mut self, group: impl Into<String>) -> Self {
        self.options.loading = Loading::Deferred { group: group.into() };
        self
    }

    /// Report a failure to resolve the prop in `rescuedProps` instead of
    /// failing the whole response.
    pub fn rescue(mut self) -> Self {
        self.options.rescue = true;
        self
    }

    /// Send the prop with every response, even partial reloads that didn't
    /// ask for it.
    pub fn always(mut self) -> Self {
        self.options.always = true;
        self
    }

    /// Merge the prop into its client-side value rather than replacing it.
    pub fn merge(mut self) -> Self {
        self.merge_options();
        self
    }

    /// Deep merge the prop into its client-side value.
    pub fn deep_merge(mut self) -> Self {
        self.merge_options().deep = true;
        self
    }

    /// Append the prop to its client-side value (the default when merging).
    pub fn append(mut self) -> Self {
        self.merge_options().append = true;
        self
    }

    /// Prepend the prop to its client-side value.
    pub fn prepend(mut self) -> Self {
        self.merge_options().append = false;
        self
    }

    /// Append the value at the given nested path, such as `data`.
    pub fn append_at(mut self, path: impl Into<String>) -> Self {
        self.merge_options().appends_at.push(path.into());
        self
    }

    /// Prepend the value at the given nested path.
    pub fn prepend_at(mut self, path: impl Into<String>) -> Self {
        self.merge_options().prepends_at.push(path.into());
        self
    }

    /// Match items on the given key while merging, updating them in place
    /// instead of duplicating them. Use a path like `data.id` for nested items.
    pub fn match_on(mut self, key: impl Into<String>) -> Self {
        self.merge_options().match_on.push(key.into());
        self
    }

    fn merge_options(&mut self) -> &mut Merge {
        self.options.merge.get_or_insert_with(Merge::default)
    }

    /// Resolve the prop once; the client remembers it on later visits.
    pub fn once(mut self) -> Self {
        self.once_options();
        self
    }

    /// Remember the once prop under the given key, so pages can share it
    /// under different prop names.
    pub fn once_as(mut self, key: impl Into<String>) -> Self {
        self.once_options().key = Some(key.into());
        self
    }

    /// Have the client forget the once prop after the given duration.
    pub fn until(mut self, ttl: Duration) -> Self {
        self.once_options().ttl = Some(ttl);
        self
    }

    /// Send the once prop even if the client already has it.
    pub fn fresh(mut self) -> Self {
        self.once_options().fresh = true;
        self
    }

    fn once_options(&mut self) -> &mut Once {
        self.options.once.get_or_insert_with(Once::default)
    }

    /// Set the key holding the items of an infinite scroll prop (`data` by default).
    pub fn wrapper(mut self, wrapper: impl Into<String>) -> Self {
        if let Some(scroll) = &mut self.options.scroll {
            scroll.wrapper = wrapper.into();
        }
        self
    }
}

impl fmt::Debug for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Value(value) => f.debug_tuple("Value").field(value).finish(),
            Self::Props(props) => f.debug_tuple("Props").field(props).finish(),
            Self::Callback(_) => f.write_str("Callback"),
            Self::Extended(_, props) => f.debug_tuple("Extended").field(props).finish(),
            Self::Failed(error) => f.debug_tuple("Failed").field(error).finish(),
        }
    }
}

/// Conversion into a [`Prop`].
///
/// Implemented for every `Serialize` type, for [`Prop`] and for nested [`Props`].
pub trait IntoProp {
    /// Convert the value into a prop.
    fn into_prop(self) -> Prop;
}

impl IntoProp for Prop {
    fn into_prop(self) -> Prop {
        self
    }
}

impl IntoProp for Props {
    fn into_prop(self) -> Prop {
        Prop::from_source(Source::Props(self))
    }
}

impl<T: Serialize> IntoProp for T {
    fn into_prop(self) -> Prop {
        Prop::value(self)
    }
}
