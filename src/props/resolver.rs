//! Resolves props and page metadata. Sibling callbacks run concurrently;
//! metadata keeps prop order for protocol compatibility.

use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::future::{BoxFuture, join_all};
use serde_json::{Map, Value};

use super::prop::Options;
use super::{Computed, Loading, Merge, Once, Prop, PropError, Props, Source};
use crate::page::{Metadata, OnceState, ScrollState};
use crate::request::Request;

pub(crate) struct PropsResolver<'a> {
    request: &'a Request,
    is_partial: bool,
    metadata: Metadata,
}

/// What to do with a prop once all of its siblings have been planned.
enum Plan {
    /// Left out of the response, though it may still announce metadata.
    Excluded { path: String, options: Options },
    /// Part of the response.
    Included {
        key: String,
        path: String,
        options: Options,
        value: Result<Resolved, PropError>,
    },
}

/// A prop value on its way into the response.
enum Resolved {
    /// A literal value; nested objects are still subject to partial filtering.
    Literal(Value),
    /// Nested props.
    Nested(Props),
    /// A callback running concurrently with its siblings, by index.
    Pending(usize),
    /// A callback's output; its children bypass partial filtering.
    Computed(Value),
}

impl<'a> PropsResolver<'a> {
    pub(crate) fn new(request: &'a Request, component: &str) -> Self {
        Self {
            request,
            is_partial: request.is_partial_reload_of(component),
            metadata: Metadata::default(),
        }
    }

    /// Resolve the shared props and page props, returning the resolved props
    /// and their metadata. Page props take precedence over shared props.
    pub(crate) async fn resolve(
        mut self,
        shared: Props,
        props: Props,
        expose_shared_keys: bool,
    ) -> Result<(Map<String, Value>, Metadata), PropError> {
        if expose_shared_keys {
            for key in shared.keys() {
                let key = key.split('.').next().unwrap_or(key);

                if !self.metadata.shared_props.iter().any(|shared| shared == key) {
                    self.metadata.shared_props.push(key.to_owned());
                }
            }
        }

        let mut combined_props = shared;
        combined_props.0.extend(props.0);

        let combined_props = unpack_dot_keys(combined_props).await?;
        let resolved = self.resolve_props(combined_props, String::new(), false).await?;

        Ok((resolved, self.metadata))
    }

    /// Recursively resolve a level of the props tree.
    ///
    /// Sibling callbacks run concurrently, while metadata is still collected
    /// in prop order for protocol compatibility.
    fn resolve_props(
        &mut self,
        props: Props,
        prefix: String,
        parent_was_resolved: bool,
    ) -> BoxFuture<'_, Result<Map<String, Value>, PropError>> {
        Box::pin(async move {
            let mut plans = Vec::with_capacity(props.len());
            let mut callbacks = Vec::new();

            for (key, Prop { source, mut options }) in props {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };

                if !self.is_included_in_partial_reload(&options, &path, parent_was_resolved) {
                    continue;
                }

                // Excluded props announce metadata without running their callbacks...
                if !self.is_partial && self.is_excluded_from_full_visit(&options, &path) {
                    plans.push(Plan::Excluded { path, options });
                    continue;
                }

                if let (Some(scroll), Some(merge)) = (&options.scroll, &mut options.merge) {
                    if self.request.prepends_scroll() {
                        merge.prepends_at.push(scroll.wrapper.clone());
                    } else {
                        merge.appends_at.push(scroll.wrapper.clone());
                    }
                }

                let value = match source {
                    Source::Value(value) => Ok(Resolved::Literal(value)),
                    Source::Props(props) => Ok(Resolved::Nested(props)),
                    Source::Failed(error) => Err(error),
                    Source::Callback(callback) => {
                        callbacks.push(callback());
                        Ok(Resolved::Pending(callbacks.len() - 1))
                    }
                };

                plans.push(Plan::Included {
                    key,
                    path,
                    options,
                    value,
                });
            }

            let mut computed: Vec<Option<Result<Computed, PropError>>> =
                join_all(callbacks).await.into_iter().map(Some).collect();

            let mut resolved = Map::new();

            for plan in plans {
                let (key, path, mut options, value) = match plan {
                    Plan::Excluded { path, options } => {
                        self.collect_excluded_metadata(&options, &path);
                        continue;
                    }
                    Plan::Included {
                        key,
                        path,
                        options,
                        value,
                    } => (key, path, options, value),
                };

                let value = value.and_then(|value| match value {
                    Resolved::Pending(index) => {
                        let Computed { value, scroll } =
                            computed[index].take().expect("each callback is taken once")?;

                        if let (Some(options), Some(metadata)) = (&mut options.scroll, scroll) {
                            options.metadata = Some(metadata);
                        }

                        Ok(Resolved::Computed(value))
                    }
                    value => Ok(value),
                });

                let value = match value {
                    Ok(value) => value,
                    Err(error) if options.rescue => {
                        tracing::error!(prop = path, %error, "rescued an Inertia prop that failed to resolve");
                        self.metadata.rescued_props.push(path);
                        continue;
                    }
                    Err(error) => return Err(error),
                };

                self.collect_metadata(&options, &path);

                let parent_was_resolved = parent_was_resolved || options.always;
                let value = match value {
                    Resolved::Nested(nested) => {
                        Value::Object(self.resolve_props(nested, path, parent_was_resolved).await?)
                    }
                    Resolved::Literal(value) => self.filter_literal(value, &path, parent_was_resolved),
                    Resolved::Computed(value) => value,
                    Resolved::Pending(_) => unreachable!("pending values are computed above"),
                };

                resolved.insert(key, value);
            }

            Ok(resolved)
        })
    }

    /// Apply partial reload filtering to the children of a literal object.
    fn filter_literal(&self, value: Value, path: &str, parent_was_resolved: bool) -> Value {
        if !self.is_partial || parent_was_resolved {
            return value;
        }

        let Value::Object(object) = value else {
            return value;
        };

        Value::Object(
            object
                .into_iter()
                .filter_map(|(key, child)| {
                    let path = format!("{path}.{key}");

                    self.matches_partial_reload(&path)
                        .then(|| (key, self.filter_literal(child, &path, false)))
                })
                .collect(),
        )
    }

    fn is_included_in_partial_reload(&self, options: &Options, path: &str, parent_was_resolved: bool) -> bool {
        !self.is_partial || options.always || parent_was_resolved || self.matches_partial_reload(path)
    }

    /// Bidirectional prefix matching against the requested paths: `auth`
    /// leads to `auth.user`, and `auth.user` is within `auth`.
    fn matches_partial_reload(&self, path: &str) -> bool {
        if let Some(only) = self.request.only()
            && !only.iter().any(|only| is_within(path, only) || is_within(only, path))
        {
            return false;
        }

        !self.is_excepted(path)
    }

    /// Whether a prop contributes metadata to a partial reload: unlike
    /// filtering, merely leading to a requested path isn't enough.
    fn contributes_partial_metadata(&self, path: &str) -> bool {
        if let Some(only) = self.request.only()
            && !only.iter().any(|only| is_within(path, only))
        {
            return false;
        }

        !self.is_excepted(path)
    }

    fn is_excepted(&self, path: &str) -> bool {
        self.request
            .except()
            .is_some_and(|except| except.iter().any(|except| is_within(path, except)))
    }

    fn is_excluded_from_full_visit(&self, options: &Options, path: &str) -> bool {
        options.loading != Loading::Eager || (self.request.is_inertia() && self.was_already_loaded(options, path))
    }

    /// Whether the client already has this once prop.
    fn was_already_loaded(&self, options: &Options, path: &str) -> bool {
        options.once.as_ref().is_some_and(|once| {
            let key = once.key.as_deref().unwrap_or(path);

            !once.fresh && self.request.except_once_props().iter().any(|loaded| loaded == key)
        })
    }

    /// Collect the metadata a prop announces while being left out.
    fn collect_excluded_metadata(&mut self, options: &Options, path: &str) {
        if let Loading::Deferred { group } = &options.loading
            && !self.was_already_loaded(options, path)
        {
            self.metadata
                .deferred_props
                .entry(group.clone())
                .or_default()
                .push(path.to_owned());
        }

        if options.loading != Loading::Eager
            && let Some(merge) = &options.merge
        {
            self.collect_merge_metadata(merge, path);
        }

        if let Some(once) = &options.once {
            self.collect_once_metadata(once, path);
        }
    }

    /// Collect the metadata of a prop that is part of the response.
    fn collect_metadata(&mut self, options: &Options, path: &str) {
        if let Some(merge) = &options.merge {
            self.collect_merge_metadata(merge, path);
        }

        if let Some(metadata) = options.scroll.as_ref().and_then(|scroll| scroll.metadata.clone()) {
            let reset = self.request.reset().iter().any(|reset| reset == path);

            self.metadata
                .scroll_props
                .insert(path.to_owned(), ScrollState { metadata, reset });
        }

        if let Some(once) = &options.once {
            self.collect_once_metadata(once, path);
        }
    }

    fn collect_merge_metadata(&mut self, merge: &Merge, path: &str) {
        if self.request.reset().iter().any(|reset| reset == path)
            || (self.is_partial && !self.contributes_partial_metadata(path))
        {
            return;
        }

        let metadata = &mut self.metadata;

        if merge.deep {
            metadata.deep_merge_props.push(path.to_owned());
        } else if merge.appends_at_root() {
            metadata.merge_props.push(path.to_owned());
        } else if merge.prepends_at_root() {
            metadata.prepend_props.push(path.to_owned());
        } else {
            metadata
                .merge_props
                .extend(merge.appends_at.iter().map(|at| format!("{path}.{at}")));
            metadata
                .prepend_props
                .extend(merge.prepends_at.iter().map(|at| format!("{path}.{at}")));
        }

        metadata
            .match_props_on
            .extend(merge.match_on.iter().map(|key| format!("{path}.{key}")));
    }

    fn collect_once_metadata(&mut self, once: &Once, path: &str) {
        if self.is_partial && !self.contributes_partial_metadata(path) {
            return;
        }

        // Expiry is expressed in milliseconds, but truncated to whole seconds...
        let expires_at = once.ttl.map(|ttl| {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();

            (now.as_secs() + ttl.as_secs()) * 1000
        });

        self.metadata.once_props.insert(
            once.key.clone().unwrap_or_else(|| path.to_owned()),
            OnceState {
                prop: path.to_owned(),
                expires_at,
            },
        );
    }
}

/// Whether `path` is `ancestor` or one of its descendants.
fn is_within(path: &str, ancestor: &str) -> bool {
    path.strip_prefix(ancestor)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
}

/// Unpack top-level dot-notation keys (`"auth.user"`) into nested props.
async fn unpack_dot_keys(mut props: Props) -> Result<Props, PropError> {
    let dotted: Vec<String> = props
        .keys()
        .filter(|key| key.contains('.'))
        .map(str::to_owned)
        .collect();

    for key in dotted {
        if let Some(prop) = props.remove(&key) {
            let segments: Vec<&str> = key.split('.').collect();
            set_nested(&mut props, &segments, prop).await?;
        }
    }

    Ok(props)
}

fn set_nested<'a>(props: &'a mut Props, segments: &'a [&'a str], prop: Prop) -> BoxFuture<'a, Result<(), PropError>> {
    Box::pin(async move {
        let Some((first, rest)) = segments.split_first() else {
            return Ok(());
        };

        if rest.is_empty() {
            props.0.insert((*first).to_owned(), prop);
            return Ok(());
        }

        let mut nested = match props.0.get_mut(*first) {
            Some(Prop {
                source: Source::Props(nested),
                ..
            }) => return set_nested(nested, rest, prop).await,
            // Resolve plain values before inserting descendants so existing siblings survive...
            Some(existing) if is_plain(&existing.options) => {
                let source = std::mem::replace(&mut existing.source, Source::Value(Value::Null));
                into_props(source).await?.unwrap_or_default()
            }
            _ => Props::new(),
        };

        set_nested(&mut nested, rest, prop).await?;
        props.insert(*first, nested);

        Ok(())
    })
}

fn is_plain(options: &Options) -> bool {
    options.loading == Loading::Eager
        && !options.always
        && options.merge.is_none()
        && options.once.is_none()
        && options.scroll.is_none()
}

/// Resolve a prop into nested props, or `None` when it isn't an object.
async fn into_props(source: Source) -> Result<Option<Props>, PropError> {
    let value = match source {
        Source::Props(props) => return Ok(Some(props)),
        Source::Value(value) => value,
        Source::Callback(callback) => callback().await?.value,
        Source::Failed(error) => return Err(error),
    };

    Ok(match value {
        Value::Object(object) => Some(object.into_iter().collect()),
        _ => None,
    })
}
