//! Resolves props and page metadata. Callbacks run concurrently, nested
//! ones included; metadata keeps prop order for protocol compatibility.

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
}

/// A level of the props tree, resolved.
type Level = (Map<String, Value>, Metadata);

/// A prop, resolved: its entry in the response, if any, and the metadata
/// it and its children announce.
struct Outcome {
    entry: Option<(String, Value)>,
    metadata: Metadata,
}

/// A prop value on its way into the response.
enum Resolved {
    /// A literal value; nested objects are still subject to partial filtering.
    Literal(Value),
    /// Nested props.
    Nested(Props),
    /// A callback's output; its children bypass partial filtering.
    Computed(Value),
}

impl<'a> PropsResolver<'a> {
    pub(crate) fn new(request: &'a Request, component: &str) -> Self {
        Self {
            request,
            is_partial: request.is_partial_reload_of(component),
        }
    }

    /// Resolve the shared props and page props, returning the resolved props
    /// and their metadata. Page props take precedence over shared props.
    pub(crate) async fn resolve(
        self,
        shared: Props,
        props: Props,
        expose_shared_keys: bool,
    ) -> Result<Level, PropError> {
        let mut shared_props: Vec<String> = Vec::new();

        if expose_shared_keys {
            for key in shared.keys() {
                let key = key.split('.').next().unwrap_or(key);

                if !shared_props.iter().any(|shared| shared == key) {
                    shared_props.push(key.to_owned());
                }
            }
        }

        let mut all = shared;
        all.0.extend(props.0);

        let all = unpack_dot_keys(all)?;
        let (resolved, mut metadata) = self.resolve_props(all, String::new(), false).await?;
        metadata.shared_props = shared_props;

        Ok((resolved, metadata))
    }

    /// Recursively resolve a level of the props tree.
    ///
    /// Every prop of the level resolves concurrently, nested levels
    /// included, while metadata is still collected in prop order so the
    /// page object matches the Laravel adapter's.
    fn resolve_props(
        &self,
        props: Props,
        prefix: String,
        parent_was_resolved: bool,
    ) -> BoxFuture<'_, Result<Level, PropError>> {
        Box::pin(async move {
            // The outcomes of excluded props, which are known up front, and
            // `None` for each included prop, whose future is in `futures`.
            let mut outcomes = Vec::with_capacity(props.len());
            let mut futures = Vec::with_capacity(props.len());

            for (key, Prop { source, mut options }) in props {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };

                // Partial reloads only include the props they ask for. Always
                // props, and the children of resolved values, are exempt.
                if !self.is_included_in_partial_reload(&options, &path, parent_was_resolved) {
                    continue;
                }

                // Optional, deferred and already loaded once props are left out
                // of full visits, without running their callbacks.
                if !self.is_partial && self.is_excluded_from_full_visit(&options, &path) {
                    let mut metadata = Metadata::default();
                    self.collect_excluded_metadata(&mut metadata, &options, &path);
                    outcomes.push(Some(Outcome { entry: None, metadata }));
                    continue;
                }

                if let (Some(scroll), Some(merge)) = (&options.scroll, &mut options.merge) {
                    if self.request.prepends_scroll() {
                        merge.prepends_at.push(scroll.wrapper.clone());
                    } else {
                        merge.appends_at.push(scroll.wrapper.clone());
                    }
                }

                outcomes.push(None);
                futures.push(self.resolve_prop(key, path, source, options, parent_was_resolved));
            }

            let mut resolved_outcomes = join_all(futures).await.into_iter();
            let mut resolved = Map::new();
            let mut metadata = Metadata::default();

            for outcome in outcomes {
                let outcome = match outcome {
                    Some(outcome) => outcome,
                    None => resolved_outcomes.next().expect("each included prop has an outcome")?,
                };

                metadata.extend(outcome.metadata);

                if let Some((key, value)) = outcome.entry {
                    resolved.insert(key, value);
                }
            }

            Ok((resolved, metadata))
        })
    }

    /// Resolve a prop that is part of the response.
    async fn resolve_prop(
        &self,
        key: String,
        path: String,
        source: Source,
        mut options: Options,
        parent_was_resolved: bool,
    ) -> Result<Outcome, PropError> {
        let mut metadata = Metadata::default();

        let value = match source {
            Source::Value(value) => Ok(Resolved::Literal(value)),
            Source::Props(props) => Ok(Resolved::Nested(props)),
            Source::Failed(error) => Err(error),
            Source::Callback(callback) => callback().await.map(|Computed { value, scroll }| {
                if let (Some(options), Some(metadata)) = (&mut options.scroll, scroll) {
                    options.metadata = Some(metadata);
                }

                Resolved::Computed(value)
            }),
            Source::Extended(callback, inserted) => callback().await.and_then(|Computed { value, .. }| {
                let mut props = object_props(value);

                for (segments, prop) in inserted {
                    set_nested(&mut props, &segments, prop)?;
                }

                Ok(Resolved::Nested(props))
            }),
        };

        let value = match value {
            Ok(value) => value,
            Err(error) if options.rescue => {
                tracing::error!(prop = path, %error, "rescued an Inertia prop that failed to resolve");
                metadata.rescued_props.push(path);
                return Ok(Outcome { entry: None, metadata });
            }
            Err(error) => return Err(error),
        };

        self.collect_metadata(&mut metadata, &options, &path);

        // Like the Laravel adapter, which adds always props back after
        // filtering, an always prop is sent whole: its children are as
        // exempt from partial filtering as a resolved value's.
        let is_whole = parent_was_resolved || options.always;

        let value = match value {
            Resolved::Nested(nested) => {
                let (nested, children) = self.resolve_props(nested, path, is_whole).await?;
                metadata.extend(children);
                Value::Object(nested)
            }
            Resolved::Literal(value) => self.filter_literal(value, &path, is_whole),
            Resolved::Computed(value) => value,
        };

        Ok(Outcome {
            entry: Some((key, value)),
            metadata,
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
    fn collect_excluded_metadata(&self, metadata: &mut Metadata, options: &Options, path: &str) {
        if let Loading::Deferred { group } = &options.loading
            && !self.was_already_loaded(options, path)
        {
            metadata
                .deferred_props
                .entry(group.clone())
                .or_default()
                .push(path.to_owned());
        }

        if options.loading != Loading::Eager
            && let Some(merge) = &options.merge
        {
            self.collect_merge_metadata(metadata, merge, path);
        }

        if let Some(once) = &options.once {
            self.collect_once_metadata(metadata, once, path);
        }
    }

    /// Collect the metadata of a prop that is part of the response.
    fn collect_metadata(&self, metadata: &mut Metadata, options: &Options, path: &str) {
        if let Some(merge) = &options.merge {
            self.collect_merge_metadata(metadata, merge, path);
        }

        if let Some(scroll) = options.scroll.as_ref().and_then(|scroll| scroll.metadata.clone()) {
            let reset = self.request.reset().iter().any(|reset| reset == path);

            metadata.scroll_props.insert(
                path.to_owned(),
                ScrollState {
                    metadata: scroll,
                    reset,
                },
            );
        }

        if let Some(once) = &options.once {
            self.collect_once_metadata(metadata, once, path);
        }
    }

    fn collect_merge_metadata(&self, metadata: &mut Metadata, merge: &Merge, path: &str) {
        if self.request.reset().iter().any(|reset| reset == path)
            || (self.is_partial && !self.contributes_partial_metadata(path))
        {
            return;
        }

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

    fn collect_once_metadata(&self, metadata: &mut Metadata, once: &Once, path: &str) {
        if self.is_partial && !self.contributes_partial_metadata(path) {
            return;
        }

        // Second precision, like the Laravel adapter. Saturating, so a TTL
        // meaning "forever" (`Duration::MAX`) can't overflow.
        let expires_at = once.ttl.map(|ttl| {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();

            now.as_secs().saturating_add(ttl.as_secs()).saturating_mul(1000)
        });

        metadata.once_props.insert(
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
fn unpack_dot_keys(mut props: Props) -> Result<Props, PropError> {
    let dotted: Vec<String> = props
        .keys()
        .filter(|key| key.contains('.'))
        .map(str::to_owned)
        .collect();

    for key in dotted {
        if let Some(prop) = props.remove(&key) {
            let segments: Vec<String> = key.split('.').map(str::to_owned).collect();
            set_nested(&mut props, &segments, prop)?;
        }
    }

    Ok(props)
}

fn set_nested(props: &mut Props, segments: &[String], prop: Prop) -> Result<(), PropError> {
    let Some((first, rest)) = segments.split_first() else {
        return Ok(());
    };

    if rest.is_empty() {
        props.0.insert(first.clone(), prop);
        return Ok(());
    }

    let mut nested = match props.0.get_mut(first) {
        Some(Prop {
            source: Source::Props(nested),
            ..
        }) => return set_nested(nested, rest, prop),
        // A plain value is unpacked so the key can be set inside it. A plain
        // callback keeps the key until it runs, so it still runs only when
        // the prop is part of the response, concurrently with its siblings.
        Some(existing) if is_plain(&existing.options) => {
            match std::mem::replace(&mut existing.source, Source::Value(Value::Null)) {
                Source::Callback(callback) => {
                    existing.source = Source::Extended(callback, vec![(rest.to_vec(), prop)]);
                    return Ok(());
                }
                Source::Extended(callback, mut inserted) => {
                    inserted.push((rest.to_vec(), prop));
                    existing.source = Source::Extended(callback, inserted);
                    return Ok(());
                }
                Source::Value(value) => object_props(value),
                Source::Failed(error) => return Err(error),
                Source::Props(_) => unreachable!("nested props are matched above"),
            }
        }
        _ => Props::new(),
    };

    set_nested(&mut nested, rest, prop)?;
    props.insert(first.clone(), nested);

    Ok(())
}

fn is_plain(options: &Options) -> bool {
    options.loading == Loading::Eager
        && !options.always
        && !options.rescue
        && options.merge.is_none()
        && options.once.is_none()
        && options.scroll.is_none()
}

/// An object's entries as props, or no props when the value isn't an object.
fn object_props(value: Value) -> Props {
    match value {
        Value::Object(object) => object.into_iter().collect(),
        _ => Props::new(),
    }
}
