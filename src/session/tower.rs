use serde_json::Value;

use super::Session;

/// Backs Inertia with [`tower_sessions::Session`], and so with any of its
/// stores (memory, Redis, SQL, signed cookies, ...).
impl Session for tower_sessions::Session {
    async fn get(&self, key: &str) -> Option<Value> {
        self.get::<Value>(key)
            .await
            .inspect_err(|error| tracing::warn!(key, %error, "failed to read from the session"))
            .ok()
            .flatten()
    }

    async fn put(&self, key: &str, value: Value) {
        if let Err(error) = self.insert(key, value).await {
            tracing::warn!(key, %error, "failed to write to the session");
        }
    }

    async fn pull(&self, key: &str) -> Option<Value> {
        // Removing marks the session modified even when the key is missing,
        // which would have every page render save the session, and so
        // overwrite changes a concurrent request made to it.
        Session::get(self, key).await?;

        self.remove::<Value>(key)
            .await
            .inspect_err(|error| tracing::warn!(key, %error, "failed to read from the session"))
            .ok()
            .flatten()
    }
}
