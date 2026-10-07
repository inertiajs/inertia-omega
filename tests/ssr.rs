//! Server-side rendering through `HttpGateway`, against stand-in SSR servers.

#![cfg(feature = "ssr")]

use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once};

use axum::Router;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use axum::routing::post;
use http::{HeaderMap, Method};
use inertia::ssr::HttpGateway;
use inertia::testing::AssertablePage;
use inertia::{Config, Inertia, Request, props};
use serde_json::json;
use tokio::net::{TcpListener, TcpStream};

/// Serve `router` on a free port, returning its URL.
async fn serve(router: Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());

    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    url
}

/// Serve `router` on a free port that starts out unreachable, returning its
/// URL and the switch that brings it up.
///
/// While it's down, connections are accepted and dropped at once. Freeing the
/// port instead, and binding it again to come up, would race with other tests
/// binding port 0.
async fn serve_switchable(router: Router) -> (String, Arc<AtomicBool>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let up = Arc::new(AtomicBool::new(false));

    // Without keep-alive, so a connection made while it's up can't reach it
    // once it's down.
    let router = router.layer(axum::middleware::map_response(|mut response: Response| async {
        response
            .headers_mut()
            .insert(header::CONNECTION, HeaderValue::from_static("close"));
        response
    }));
    let listener = Switchable {
        listener,
        up: up.clone(),
    };
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    (url, up)
}

/// A listener that drops the connections it accepts while it's down.
struct Switchable {
    listener: TcpListener,
    up: Arc<AtomicBool>,
}

impl axum::serve::Listener for Switchable {
    type Io = TcpStream;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (TcpStream, SocketAddr) {
        loop {
            let accepted = axum::serve::Listener::accept(&mut self.listener).await;

            if self.up.load(Ordering::SeqCst) {
                return accepted;
            }
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }
}

/// The body of a first visit, rendered through the SSR server at `url`.
async fn first_visit(url: &str) -> String {
    let config = Config::new().ssr(HttpGateway::new().url(url));
    let request = Request::new(Method::GET, &"/".parse().unwrap(), HeaderMap::new());

    let response = Inertia::new(config, request)
        .render("Home", props! { "name" => "Taylor" })
        .into_http()
        .await;

    response.into_body()
}

#[tokio::test]
async fn pages_are_rendered_by_the_ssr_server() {
    let url = serve(Router::new().route(
        "/render",
        post(|| async {
            axum::Json(json!({ "head": ["<title>Home</title>"], "body": "<div id=\"app\">Taylor</div>" }))
        }),
    ))
    .await;

    let body = first_visit(&url).await;

    assert!(body.contains("<title>Home</title>"), "{body}");
    assert!(body.contains(r#"<div id="app">Taylor</div>"#), "{body}");
}

#[tokio::test]
async fn failed_renders_fall_back_to_the_client() {
    let url = serve(Router::new().route("/render", post(|| async { StatusCode::INTERNAL_SERVER_ERROR }))).await;

    let body = first_visit(&url).await;

    assert!(body.contains(r#"<div id="app"></div>"#), "{body}");
    AssertablePage::from_body(&body)
        .component("Home")
        .equals("name", "Taylor");
}

#[tokio::test]
async fn unreachable_servers_fall_back_to_the_client() {
    let (url, _up) = serve_switchable(Router::new()).await;

    let body = first_visit(&url).await;

    assert!(body.contains(r#"<div id="app"></div>"#), "{body}");
    AssertablePage::from_body(&body)
        .component("Home")
        .equals("name", "Taylor");
}

#[tokio::test]
async fn unreachable_servers_are_warned_about_again_after_coming_back() {
    let router = Router::new().route("/render", post(|| async { axum::Json(json!({ "body": "Taylor" })) }));
    let (url, up) = serve_switchable(router).await;

    let warnings = unreachable_warnings(format!("{url}/render"));

    first_visit(&url).await;
    first_visit(&url).await;
    assert_eq!(warnings(), 1, "warned once while down");

    up.store(true, Ordering::SeqCst);
    assert!(first_visit(&url).await.contains("Taylor"));

    up.store(false, Ordering::SeqCst);
    first_visit(&url).await;
    assert_eq!(warnings(), 2, "warned again once down again");
}

/// Start recording Inertia's warnings, returning a count of those about the
/// SSR server at `url` being unreachable.
///
/// Recorded by a global subscriber: one set for a test's thread would race
/// with other tests hitting the same log statement first.
fn unreachable_warnings(url: String) -> impl Fn() -> usize {
    static WARNED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static INSTALL: Once = Once::new();

    INSTALL.call_once(|| tracing::subscriber::set_global_default(RecordWarnings(&WARNED)).unwrap());

    move || WARNED.lock().unwrap().iter().filter(|warned| **warned == url).count()
}

/// Records the `url` of each warning Inertia logs.
struct RecordWarnings(&'static Mutex<Vec<String>>);

impl tracing::Subscriber for RecordWarnings {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        struct Url(Option<String>);

        impl tracing::field::Visit for Url {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "url" {
                    self.0 = Some(format!("{value:?}"));
                }
            }
        }

        let metadata = event.metadata();

        if *metadata.level() == tracing::Level::WARN && metadata.target().starts_with("inertia") {
            let mut url = Url(None);
            event.record(&mut url);
            self.0.lock().unwrap().extend(url.0);
        }
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}
