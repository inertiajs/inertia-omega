//! The [Axum](https://docs.rs/axum) adapter.
//!
//! Install an [`InertiaLayer`] on the router, then take [`Inertia`] as an
//! extractor in handlers and return the [`Response`] of a render:
//!
//! ```no_run
//! use axum::{Router, routing::get};
//! use inertia::axum::InertiaLayer;
//! use inertia::{Config, Inertia, props};
//! use tower_sessions::{MemoryStore, SessionManagerLayer};
//!
//! async fn home(inertia: Inertia) -> inertia::Response {
//!     inertia.render("Home", props! { name => "Taylor" })
//! }
//!
//! let app: Router = Router::new()
//!     .route("/", get(home))
//!     .layer(InertiaLayer::new(Config::new().version("1")))
//!     // The session layer wraps Inertia so the session is available during rendering...
//!     .layer(SessionManagerLayer::new(MemoryStore::default()));
//! ```
//!
//! The layer resolves the props of a returned render, applies the protocol's
//! redirect and versioning rules, and writes flash data and validation
//! errors to the session. With the `tower-sessions` feature it uses
//! `tower_sessions::Session`; [`InertiaLayer::session`] plugs in another.
//!
//! [`Inertia`]: crate::Inertia
//! [`Response`]: crate::Response

mod extract;
mod layer;
mod response;

pub use self::extract::MissingInertiaLayer;
pub use self::layer::{InertiaLayer, InertiaService};
pub use self::response::is_render;
