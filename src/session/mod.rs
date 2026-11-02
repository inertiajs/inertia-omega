//! The session Inertia keeps flash data, validation errors and history
//! flags in between requests.

#[cfg(feature = "tower-sessions")]
mod tower;

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};

use futures_util::future::BoxFuture;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::errors::ErrorBags;

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

/// What one request leaves in the session for the next page render.
#[derive(Default)]
pub(crate) struct Handoff {
    pub(crate) flash: Map<String, Value>,
    pub(crate) errors: ErrorBags,
    pub(crate) clear_history: bool,
    pub(crate) preserve_fragment: bool,
}

impl Handoff {
    /// Take what earlier requests left, so it's delivered to this page only.
    pub(crate) async fn pull(session: Option<&SharedSession>) -> Self {
        let Some(session) = session else {
            return Self::default();
        };

        Self {
            flash: decode(session.pull(key::FLASH).await),
            errors: decode(session.pull(key::ERRORS).await),
            clear_history: decode(session.pull(key::CLEAR_HISTORY).await),
            preserve_fragment: decode(session.pull(key::PRESERVE_FRAGMENT).await),
        }
    }

    /// Add a later handoff's data, whose flash data wins on conflicts.
    pub(crate) fn merge(&mut self, later: Self) {
        self.flash.extend(later.flash);
        self.errors.merge(later.errors);
        self.clear_history |= later.clear_history;
        self.preserve_fragment |= later.preserve_fragment;
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.flash.is_empty() && self.errors.is_empty() && !self.clear_history && !self.preserve_fragment
    }

    /// Store the data on top of what earlier requests left.
    pub(crate) async fn save(self, session: &SharedSession) {
        if !self.flash.is_empty() {
            let mut flash: Map<String, Value> = decode(session.get(key::FLASH).await);
            flash.extend(self.flash);
            session.put(key::FLASH, Value::Object(flash)).await;
        }

        if !self.errors.is_empty() {
            let mut errors: ErrorBags = decode(session.get(key::ERRORS).await);
            errors.merge(self.errors);
            session
                .put(key::ERRORS, serde_json::to_value(errors).unwrap_or_default())
                .await;
        }

        if self.clear_history {
            session.put(key::CLEAR_HISTORY, Value::Bool(true)).await;
        }

        if self.preserve_fragment {
            session.put(key::PRESERVE_FRAGMENT, Value::Bool(true)).await;
        }
    }
}

/// A stored value, or the default when it's missing or no longer decodes.
fn decode<T: DeserializeOwned + Default>(value: Option<Value>) -> T {
    value
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

/// An in-memory session. Clones share their data.
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
