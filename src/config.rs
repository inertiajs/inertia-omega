//! Application-wide Inertia configuration.

use std::fmt;
use std::sync::Arc;

use crate::props::Props;
use crate::request::Request;
use crate::ssr::{DynGateway, Gateway};
use crate::view::{RootView, View, default_root_view};

type Share = dyn Fn(&Request) -> Props + Send + Sync;
type ResolveUrl = dyn Fn(&Request) -> String + Send + Sync;

/// How Inertia renders pages for an application.
///
/// ```
/// use inertia::{Config, props};
///
/// let config = Config::new()
///     .version("1")
///     .share(|_request| props! { "appName" => "Ferris" })
///     .root_view(|view: &inertia::View<'_>| {
///         format!("<!DOCTYPE html><html><head>{}</head><body>{}</body></html>", view.head, view.body)
///     });
/// ```
#[derive(Clone)]
#[must_use]
pub struct Config {
    version: Version,
    root_view: Arc<dyn RootView>,
    pub(crate) root_id: String,
    pub(crate) shares: Vec<Arc<Share>>,
    pub(crate) url_resolver: Option<Arc<ResolveUrl>>,
    pub(crate) gateway: Option<Arc<dyn DynGateway>>,
    pub(crate) encrypt_history: bool,
    pub(crate) expose_shared_prop_keys: bool,
    pub(crate) preserve_big_integers: bool,
    pub(crate) with_all_errors: bool,
}

#[derive(Clone)]
enum Version {
    Fixed(String),
    Computed(Arc<dyn Fn() -> String + Send + Sync>),
}

impl Config {
    /// A configuration with the defaults.
    pub fn new() -> Self {
        Self {
            version: Version::Fixed(String::new()),
            root_view: Arc::new(default_root_view),
            root_id: "app".to_owned(),
            shares: Vec::new(),
            url_resolver: None,
            gateway: None,
            encrypt_history: false,
            expose_shared_prop_keys: true,
            preserve_big_integers: false,
            with_all_errors: false,
        }
    }

    /// Set the asset version. When a client is running another version, its
    /// next visit becomes a full page load so it picks up the new assets.
    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = Version::Fixed(version.into());
        self
    }

    /// Compute the asset version on every request, such as from a hash of
    /// the build manifest.
    pub fn version_with(mut self, version: impl Fn() -> String + Send + Sync + 'static) -> Self {
        self.version = Version::Computed(Arc::new(version));
        self
    }

    /// The current asset version.
    pub fn current_version(&self) -> String {
        match &self.version {
            Version::Fixed(version) => version.clone(),
            Version::Computed(version) => version(),
        }
    }

    /// Set the template for the HTML document of first visits.
    pub fn root_view(mut self, root_view: impl RootView) -> Self {
        self.root_view = Arc::new(root_view);
        self
    }

    pub(crate) fn render_root_view(&self, view: &View<'_>) -> String {
        self.root_view.render(view)
    }

    /// Set the id of the element the app mounts in (`app` by default).
    pub fn root_id(mut self, id: impl Into<String>) -> Self {
        self.root_id = id.into();
        self
    }

    /// Share props with every page. The callback runs on every render, so
    /// make anything expensive a [lazy](crate::lazy) prop.
    ///
    /// For per-request data, such as the current user, call
    /// [`Inertia::share`](crate::Inertia::share) from a middleware instead.
    pub fn share(mut self, share: impl Fn(&Request) -> Props + Send + Sync + 'static) -> Self {
        self.shares.push(Arc::new(share));
        self
    }

    /// Customize the `url` of the page object.
    pub fn resolve_url_using(mut self, resolve: impl Fn(&Request) -> String + Send + Sync + 'static) -> Self {
        self.url_resolver = Some(Arc::new(resolve));
        self
    }

    /// Render first visits on the server through the given [`Gateway`].
    pub fn ssr(mut self, gateway: impl Gateway) -> Self {
        self.gateway = Some(Arc::new(gateway));
        self
    }

    /// Encrypt every page in the browser history.
    pub fn encrypt_history(mut self, encrypt: bool) -> Self {
        self.encrypt_history = encrypt;
        self
    }

    /// List the shared props in the `sharedProps` field of the page (on by default).
    pub fn expose_shared_prop_keys(mut self, expose: bool) -> Self {
        self.expose_shared_prop_keys = expose;
        self
    }

    /// Send integers outside JavaScript's safe range so the client revives
    /// them as `BigInt`s.
    pub fn preserve_big_integers(mut self, preserve: bool) -> Self {
        self.preserve_big_integers = preserve;
        self
    }

    /// Share every validation message per field, instead of only the first.
    pub fn with_all_errors(mut self, all: bool) -> Self {
        self.with_all_errors = all;
        self
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            // Not `current_version`: formatting shouldn't run the app's
            // callback, which may hash a file.
            .field(
                "version",
                &match &self.version {
                    Version::Fixed(version) => version.as_str(),
                    Version::Computed(_) => "<computed>",
                },
            )
            .field("root_id", &self.root_id)
            .field("ssr", &self.gateway.is_some())
            .field("encrypt_history", &self.encrypt_history)
            .field("expose_shared_prop_keys", &self.expose_shared_prop_keys)
            .field("preserve_big_integers", &self.preserve_big_integers)
            .field("with_all_errors", &self.with_all_errors)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_leaves_computed_versions_alone() {
        let config = Config::new().version_with(|| panic!("the version was computed"));

        assert!(format!("{config:?}").contains(r#"version: "<computed>""#));
        assert!(format!("{:?}", Config::new().version("1")).contains(r#"version: "1""#));
    }
}
