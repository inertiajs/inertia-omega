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
    /// Create metadata for page-number based pagination.
    pub fn pages(page_name: impl Into<String>, current_page: u64, last_page: u64) -> Self {
        Self {
            page_name: page_name.into(),
            previous_page: (current_page > 1).then(|| (current_page - 1).into()),
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

/// A page of items, serialized like Laravel's length-aware paginator.
#[derive(Debug, Clone, Serialize)]
#[must_use]
pub struct Paginator<T> {
    /// The items on the current page.
    pub data: Vec<T>,
    /// The current page number.
    pub current_page: u64,
    /// The last page number.
    pub last_page: u64,
    /// The number of items per page.
    pub per_page: u64,
    /// The total number of items.
    pub total: u64,
    /// The 1-based index of the first item on the page.
    pub from: Option<u64>,
    /// The 1-based index of the last item on the page.
    pub to: Option<u64>,
    #[serde(skip)]
    page_name: String,
}

impl<T> Paginator<T> {
    /// Create a paginator for a page of items.
    /// Page numbers and page sizes have a minimum of one.
    pub fn new(data: Vec<T>, total: u64, per_page: u64, current_page: u64) -> Self {
        let current_page = current_page.max(1);
        let per_page = per_page.max(1);
        let last_page = total.div_ceil(per_page).max(1);
        let count = data.len() as u64;
        let from = (count > 0).then(|| (current_page - 1).saturating_mul(per_page).saturating_add(1));

        Self {
            data,
            current_page,
            last_page,
            per_page,
            total,
            from,
            to: from.map(|from| from.saturating_add(count - 1)),
            page_name: "page".to_owned(),
        }
    }

    /// Paginate a full collection of items.
    pub fn from_items(items: impl IntoIterator<Item = T>, per_page: u64, page: u64) -> Self {
        let items: Vec<T> = items.into_iter().collect();
        let page = page.max(1);
        let per_page = per_page.max(1);
        let total = items.len() as u64;
        let skip = usize::try_from((page - 1).saturating_mul(per_page)).unwrap_or(usize::MAX);
        let take = usize::try_from(per_page).unwrap_or(usize::MAX);
        let data = items.into_iter().skip(skip).take(take).collect();

        Self::new(data, total, per_page, page)
    }

    /// Set the query string parameter holding the page.
    pub fn page_name(mut self, page_name: impl Into<String>) -> Self {
        self.page_name = page_name.into();
        self
    }

    /// Determine if there are more pages after this one.
    pub fn has_more_pages(&self) -> bool {
        self.current_page < self.last_page
    }
}

impl<T> ProvidesScrollMetadata for Paginator<T> {
    fn scroll_metadata(&self) -> ScrollMetadata {
        ScrollMetadata::pages(&self.page_name, self.current_page, self.last_page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_zero_is_the_first_page() {
        let paginator = Paginator::new(vec![1, 2], 2, 10, 0);

        assert_eq!(paginator.current_page, 1);
        assert_eq!((paginator.from, paginator.to), (Some(1), Some(2)));
        assert_eq!(paginator.scroll_metadata().previous_page, None);
    }
}
