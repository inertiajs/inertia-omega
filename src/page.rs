//! The page object: the JSON document that drives the Inertia client.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::props::ScrollMetadata;

/// The page object.
///
/// It is embedded in the HTML of the first visit and returned as JSON for
/// subsequent Inertia visits. Fields serialize in protocol order, and
/// optional metadata is omitted when empty.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    /// The page component name.
    pub component: String,
    /// The resolved props.
    pub props: Map<String, Value>,
    /// The page URL.
    pub url: String,
    /// The current asset version.
    pub version: String,
    /// How the client should treat the props.
    #[serde(flatten)]
    pub metadata: Metadata,
    /// Whether `{"$bigint": "..."}` markers should be revived as `BigInt`s.
    #[serde(default, skip_serializing_if = "is_false")]
    pub preserve_big_integers: bool,
    /// Whether the client should clear its history.
    #[serde(default, skip_serializing_if = "is_false")]
    pub clear_history: bool,
    /// Whether the client should encrypt this page in its history.
    #[serde(default, skip_serializing_if = "is_false")]
    pub encrypt_history: bool,
    /// One-time data that isn't kept in the browser history.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub flash: Map<String, Value>,
    /// Whether the client should keep the URL fragment across a redirect.
    #[serde(default, skip_serializing_if = "is_false")]
    pub preserve_fragment: bool,
}

/// The prop metadata collected while resolving a page's props.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    /// The top-level keys of the shared props.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared_props: Vec<String>,
    /// The props to append to their client-side value.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merge_props: Vec<String>,
    /// The props to prepend to their client-side value.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prepend_props: Vec<String>,
    /// The props to deep merge with their client-side value.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deep_merge_props: Vec<String>,
    /// The `prop.key` paths used to match items while merging.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub match_props_on: Vec<String>,
    /// The deferred props, by the group they're loaded in.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub deferred_props: IndexMap<String, Vec<String>>,
    /// The deferred props that failed to resolve and were rescued.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rescued_props: Vec<String>,
    /// The pagination state of infinite scroll props.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub scroll_props: IndexMap<String, ScrollState>,
    /// The props the client should remember across visits.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub once_props: IndexMap<String, OnceState>,
}

impl Metadata {
    /// Append metadata collected after this, as if it had been collected
    /// into this directly.
    pub(crate) fn extend(&mut self, other: Self) {
        let Self {
            shared_props,
            merge_props,
            prepend_props,
            deep_merge_props,
            match_props_on,
            deferred_props,
            rescued_props,
            scroll_props,
            once_props,
        } = other;

        self.shared_props.extend(shared_props);
        self.merge_props.extend(merge_props);
        self.prepend_props.extend(prepend_props);
        self.deep_merge_props.extend(deep_merge_props);
        self.match_props_on.extend(match_props_on);
        for (group, props) in deferred_props {
            self.deferred_props.entry(group).or_default().extend(props);
        }
        self.rescued_props.extend(rescued_props);
        self.scroll_props.extend(scroll_props);
        self.once_props.extend(once_props);
    }
}

/// The pagination state of an infinite scroll prop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScrollState {
    /// The pagination metadata.
    #[serde(flatten)]
    pub metadata: ScrollMetadata,
    /// Whether the client should discard the pages it has loaded.
    pub reset: bool,
}

/// A prop the client should remember across visits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OnceState {
    /// The path of the prop.
    pub prop: String,
    /// When the client should forget the prop, in milliseconds since the epoch.
    pub expires_at: Option<u64>,
}

fn is_false(value: &bool) -> bool {
    !value
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_minimal_page_only_has_the_required_fields() {
        let page = Page {
            component: "Home".into(),
            url: "/".into(),
            version: "1".into(),
            ..Page::default()
        };

        assert_eq!(
            serde_json::to_value(&page).unwrap(),
            json!({ "component": "Home", "props": {}, "url": "/", "version": "1" })
        );
    }

    #[test]
    fn metadata_serializes_in_protocol_order() {
        let mut page = Page::default();
        page.metadata.once_props.insert(
            "plans".into(),
            OnceState {
                prop: "plans".into(),
                expires_at: None,
            },
        );
        page.metadata.merge_props.push("posts".into());
        page.clear_history = true;

        let json = serde_json::to_string(&page).unwrap();

        assert!(json.find("mergeProps") < json.find("onceProps"));
        assert!(json.find("onceProps") < json.find("clearHistory"));
        assert!(json.contains(r#""expiresAt":null"#));
    }

    #[test]
    fn round_trips_through_json() {
        let page: Page = serde_json::from_value(json!({
            "component": "Users/Index",
            "props": { "users": [] },
            "url": "/users",
            "version": "1",
            "deferredProps": { "default": ["stats"] },
            "scrollProps": {
                "users": { "pageName": "page", "previousPage": null, "nextPage": 2, "currentPage": 1, "reset": false }
            },
        }))
        .unwrap();

        assert_eq!(page.metadata.deferred_props["default"], ["stats"]);
        assert_eq!(page.metadata.scroll_props["users"].metadata.next_page, Some(json!(2)));
    }
}
