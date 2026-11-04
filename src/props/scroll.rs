//! Pagination metadata for infinite scroll props.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The pagination metadata the client's `<InfiniteScroll>` component uses to
/// request the previous and next pages.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrollMetadata {
    /// The query string parameter holding the page.
    pub page_name: String,
    /// The previous page identifier, if any.
    pub previous_page: Option<Value>,
    /// The next page identifier, if any.
    pub next_page: Option<Value>,
    /// The current page identifier.
    pub current_page: Option<Value>,
}

impl ScrollMetadata {
    /// Create metadata for page-number based pagination. Past the last page,
    /// the previous page is the last page, so the client doesn't load every
    /// empty page in between.
    pub fn pages(page_name: impl Into<String>, current_page: u64, last_page: u64) -> Self {
        Self {
            page_name: page_name.into(),
            previous_page: (current_page > 1).then(|| (current_page - 1).min(last_page.max(1)).into()),
            next_page: (current_page < last_page).then(|| (current_page + 1).into()),
            current_page: Some(current_page.into()),
        }
    }

    /// Create metadata for cursor based pagination.
    pub fn cursors(
        cursor_name: impl Into<String>,
        previous: Option<String>,
        next: Option<String>,
        current: Option<String>,
    ) -> Self {
        Self {
            page_name: cursor_name.into(),
            previous_page: previous.map(Value::String),
            next_page: next.map(Value::String),
            current_page: Some(current.map_or(Value::from(1), Value::String)),
        }
    }
}

/// A value that can describe its own pagination state.
pub trait ProvidesScrollMetadata {
    /// Get the pagination metadata.
    fn scroll_metadata(&self) -> ScrollMetadata;
}

impl ProvidesScrollMetadata for ScrollMetadata {
    fn scroll_metadata(&self) -> ScrollMetadata {
        self.clone()
    }
}
