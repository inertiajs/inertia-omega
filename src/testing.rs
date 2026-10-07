//! Assertions for testing Inertia pages, inspired by Laravel's `assertInertia`.
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

use serde::Serialize;
use serde_json::Value;

use crate::page::Page;

/// A page under test. Each assertion panics with a descriptive message when it fails.
#[derive(Debug, Clone)]
pub struct AssertablePage {
    page: Page,
}

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
            Ok(page) => Self { page },
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

    /// Assert the page component.
    #[track_caller]
    pub fn component(&self, expected: &str) -> &Self {
        assert_eq!(self.page.component, expected, "unexpected Inertia page component");
        self
    }

    /// Assert the page URL.
    #[track_caller]
    pub fn url(&self, expected: &str) -> &Self {
        assert_eq!(self.page.url, expected, "unexpected Inertia page URL");
        self
    }

    /// Assert the asset version.
    #[track_caller]
    pub fn version(&self, expected: &str) -> &Self {
        assert_eq!(self.page.version, expected, "unexpected Inertia asset version");
        self
    }

    /// Assert that the prop exists.
    #[track_caller]
    pub fn has(&self, path: &str) -> &Self {
        assert!(
            self.prop(path).is_some(),
            "expected the Inertia page to have prop [{path}]\n\n{}",
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
            Some(other) => panic!("expected Inertia prop [{path}] to be an array or object, got {other}"),
            None => panic!("expected the Inertia page to have prop [{path}]\n\n{}", self.props()),
        };

        assert_eq!(actual, count, "unexpected number of items in Inertia prop [{path}]");
        self
    }

    /// Assert that the prop doesn't exist.
    #[track_caller]
    pub fn missing(&self, path: &str) -> &Self {
        assert!(
            self.prop(path).is_none(),
            "expected the Inertia page not to have prop [{path}]\n\n{}",
            self.props()
        );
        self
    }

    /// Assert that the prop equals the given value.
    #[track_caller]
    pub fn equals(&self, path: &str, expected: impl Serialize) -> &Self {
        let expected = serde_json::to_value(expected).expect("the expected value should serialize");

        match self.prop(path) {
            Some(actual) => assert_eq!(actual, &expected, "unexpected value of Inertia prop [{path}]"),
            None => panic!("expected the Inertia page to have prop [{path}]\n\n{}", self.props()),
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

        assert_eq!(deferred, props, "unexpected deferred props in group [{group}]");
        self
    }

    /// Assert the page's flash data.
    #[track_caller]
    pub fn flash(&self, key: &str, expected: impl Serialize) -> &Self {
        let expected = serde_json::to_value(expected).expect("the expected value should serialize");

        assert_eq!(
            self.page.flash.get(key),
            Some(&expected),
            "unexpected Inertia flash data [{key}]"
        );
        self
    }

    fn props(&self) -> String {
        serde_json::to_string_pretty(&self.page.props).unwrap_or_default()
    }
}

/// The contents of the page data `<script>` of an HTML document.
#[track_caller]
fn page_script(html: &str) -> &str {
    // The script with `data-page`, not just any JSON script before it.
    let Some((json, _)) = html
        .split_once("<script data-page=")
        .and_then(|(_, rest)| rest.split_once('>'))
        .and_then(|(_, rest)| rest.split_once("</script>"))
    else {
        // Not in a closure, which `#[track_caller]` doesn't reach.
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

    #[test]
    fn skips_other_json_scripts() {
        let html = format!(r#"<head><script type="application/json">{{"component":"Other"}}</script></head>{HTML}"#);

        AssertablePage::from_body(&html).component("Home");
    }

    #[test]
    #[should_panic(expected = "unexpected Inertia page component")]
    fn fails_on_the_wrong_component() {
        AssertablePage::from_body(HTML).component("Dashboard");
    }
}
