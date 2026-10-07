//! The session Inertia keeps flash data, validation errors and history
//! flags in between requests.

#[cfg(feature = "tower-sessions")]
mod tower;

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};

use futures_util::future::BoxFuture;
use serde_json::Value;

/// The session keys used by Inertia.
pub mod key {
    /// Flash data for the next page.
    pub const FLASH: &str = "inertia.flash_data";

    /// Validation error bags for the next page.
    pub const ERRORS: &str = "inertia.errors";

    /// Whether the next page should clear the browser history.
    pub const CLEAR_HISTORY: &str = "inertia.clear_history";

    /// Whether the next page should keep the URL fragment of its visit.
    pub const PRESERVE_FRAGMENT: &str = "inertia.preserve_fragment";
}

/// A session store, scoped to the current request.
///
/// Implement it to back Inertia with your framework's session. With the
/// `tower-sessions` feature it is implemented for `tower_sessions::Session`.
/// Failures should be logged rather than surfaced: losing a flash message
/// shouldn't fail a page.
pub trait Session: Send + Sync + 'static {
    /// Get a value.
    fn get(&self, key: &str) -> impl Future<Output = Option<Value>> + Send;

    /// Store a value.
    fn put(&self, key: &str, value: Value) -> impl Future<Output = ()> + Send;

    /// Get a value and remove it from the session.
    ///
    /// Inertia only pulls the keys it found with [`get`](Session::get), but a
    /// session that tracks changes should still leave itself unmodified when
    /// the key is missing.
    fn pull(&self, key: &str) -> impl Future<Output = Option<Value>> + Send;
}

/// An object-safe [`Session`], so a request can hold any session type.
pub(crate) trait DynSession: Send + Sync {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Option<Value>>;
    fn put<'a>(&'a self, key: &'a str, value: Value) -> BoxFuture<'a, ()>;
    fn pull<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Option<Value>>;
}

impl<S: Session> DynSession for S {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Option<Value>> {
        Box::pin(Session::get(self, key))
    }

    fn put<'a>(&'a self, key: &'a str, value: Value) -> BoxFuture<'a, ()> {
        Box::pin(Session::put(self, key, value))
    }

    fn pull<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Option<Value>> {
        Box::pin(Session::pull(self, key))
    }
}

pub(crate) type SharedSession = Arc<dyn DynSession>;

/// An in-memory session, like Laravel's `array` driver. Clones share their data.
///
/// Useful in tests, or as a starting point for an adapter.
#[derive(Debug, Clone, Default)]
pub struct ArraySession {
    values: Arc<Mutex<HashMap<String, Value>>>,
}

impl ArraySession {
    /// Create an empty session.
    pub fn new() -> Self {
        Self::default()
    }

    fn values(&self) -> std::sync::MutexGuard<'_, HashMap<String, Value>> {
        self.values.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Session for ArraySession {
    async fn get(&self, key: &str) -> Option<Value> {
        self.values().get(key).cloned()
    }

    async fn put(&self, key: &str, value: Value) {
        self.values().insert(key.to_owned(), value);
    }

    async fn pull(&self, key: &str) -> Option<Value> {
        self.values().remove(key)
    }
}
