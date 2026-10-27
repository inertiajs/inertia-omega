//! # Inertia.js for Rust
//!
//! The server side of the [Inertia.js](https://inertiajs.com) protocol:
//! build single-page apps with React, Vue or Svelte, routed and rendered by
//! your Rust server, without building an API.
//!
//! The core is framework-agnostic, built on the `http` crate. It resolves
//! props, builds the page object, renders the root view, talks to the SSR
//! server and implements the protocol's redirect and versioning rules.
//! Adapters connect it to a web framework; the `axum` feature supplies an Axum adapter.
//!
//! ```no_run
//! # #[cfg(feature = "axum")]
//! # {
//! use axum::{Router, routing::get};
//! use inertia::{Config, Inertia, axum::InertiaLayer, props};
//!
//! async fn dashboard(inertia: Inertia) -> inertia::Response {
//!     inertia.render("Dashboard", props! {
//!         user => "Taylor",
//!         stats => inertia::defer(|| async { vec![1, 2, 3] }),
//!     })
//! }
//!
//! let app: Router = Router::new()
//!     .route("/", get(dashboard))
//!     .layer(InertiaLayer::new(Config::new().version("1")));
//! # }
//! ```
//!
//! ## Features
//!
//! - `axum` (default): the Axum adapter.
//! - `tower-sessions` (default): a [`Session`] for `tower_sessions::Session`.
//! - `ssr` (default): `ssr::HttpGateway`, for server-side rendering over HTTP.
//! - `validator`, `garde`: convert their errors into [`ValidationErrors`].

#![warn(missing_docs)]

mod config;
mod errors;
#[cfg(feature = "ssr")]
mod files;
pub mod header;
mod inertia;
mod json;
mod page;
pub mod props;
pub mod protocol;
mod request;
mod response;
pub mod session;
pub mod ssr;
pub mod testing;
mod view;

#[cfg(feature = "axum")]
pub mod axum;

pub use crate::config::Config;
pub use crate::errors::ValidationErrors;
pub use crate::inertia::Inertia;
pub use crate::json::{encode_big_integers, html_safe_json};
pub use crate::page::{Metadata, OnceState, Page, ScrollState};
pub use crate::props::{
    IntoProp, IntoProps, Paginator, Prop, PropError, Props, ProvidesScrollMetadata, ScrollMetadata, always, deep_merge,
    defer, lazy, merge, once, optional, scroll, scroll_with, try_lazy,
};
pub use crate::protocol::redirect;
pub use crate::request::Request;
pub use crate::response::Response;
pub use crate::session::Session;
pub use crate::view::{RootView, View};

/// The HTTP response type of the framework-agnostic core.
pub type HttpResponse = http::Response<String>;

#[doc(hidden)]
pub mod __private {
    pub use inertia_omega_macros::props;

    /// A value that a shorthand `props!` entry can borrow, which is any `Serialize` value.
    #[diagnostic::on_unimplemented(
        message = "a shorthand `props!` entry borrows its variable, and `{Self}` isn't `Serialize`",
        label = "borrowed here",
        note = "to move a `Prop` or `Props`, such as one from `inertia::lazy` or `inertia::defer`, write `name => name`"
    )]
    pub trait Shorthand {}

    impl<T: serde::Serialize + ?Sized> Shorthand for T {}

    pub fn shorthand<T: Shorthand + ?Sized>(value: &T) -> &T {
        value
    }
}
