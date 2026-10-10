//! The README's quick start: `cargo run --example quick_start`, then open
//! http://127.0.0.1:3000 or request it with an `X-Inertia: true` header.

use axum::{Router, routing::get};
use inertia::axum::InertiaLayer;
use inertia::{Config, Inertia, props};
use tower_sessions::{MemoryStore, SessionManagerLayer};

async fn home(inertia: Inertia) -> inertia::Response {
    inertia.render("Home", props! { "greeting" => "Hello from Rust" })
}

#[tokio::main]
async fn main() {
    let config = Config::new().version("1").root_view(|view: &inertia::View<'_>| {
        format!(
            r#"<!DOCTYPE html>
<html>
<head>
    <script type="module" src="/build/app.js"></script>
    {head}
</head>
<body>{body}</body>
</html>"#,
            head = view.head,
            body = view.body,
        )
    });

    let app = Router::new()
        .route("/", get(home))
        .layer(InertiaLayer::new(config))
        // The session layer wraps Inertia so the session is available during rendering...
        .layer(SessionManagerLayer::new(MemoryStore::default()).with_secure(false));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
