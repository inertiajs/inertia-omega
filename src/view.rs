//! The root view: the HTML document that hosts the app on the first visit.

use serde_json::{Map, Value};

use crate::page::Page;

/// Renders the HTML document of a first, full page visit.
///
/// Implemented for closures, so a root view is usually a template function:
///
/// ```
/// let root_view = |view: &inertia::View<'_>| {
///     format!(
///         "<!DOCTYPE html><html><head>{head}</head><body>{body}</body></html>",
///         head = view.head,
///         body = view.body,
///     )
/// };
///
/// let config = inertia::Config::new().root_view(root_view);
/// ```
pub trait RootView: Send + Sync + 'static {
    /// Render the document.
    fn render(&self, view: &View<'_>) -> String;
}

impl<F> RootView for F
where
    F: Fn(&View<'_>) -> String + Send + Sync + 'static,
{
    fn render(&self, view: &View<'_>) -> String {
        self(view)
    }
}

/// The page, head elements, app markup and template data a [`RootView`] renders.
#[derive(Debug)]
pub struct View<'a> {
    /// The page object.
    pub page: &'a Page,
    /// The `<head>` elements rendered by SSR, or an empty string.
    pub head: &'a str,
    /// The app: the page data `<script>` and the element the app mounts in,
    /// server-side rendered when SSR is enabled.
    pub body: &'a str,
    /// Data for the template only, from [`Response::with_view_data`](crate::Response::with_view_data).
    pub data: &'a Map<String, Value>,
    /// Whether the page was server-side rendered.
    pub ssr: bool,
}

/// A bare-bones document, used when no root view is configured.
pub(crate) fn default_root_view(view: &View<'_>) -> String {
    format!(
        concat!(
            "<!DOCTYPE html>\n<html>\n<head>\n",
            "<meta charset=\"utf-8\">\n",
            "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n",
            "{head}\n</head>\n<body>\n{body}\n</body>\n</html>\n",
        ),
        head = view.head,
        body = view.body,
    )
}
