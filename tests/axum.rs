//! The Axum adapter.

#![cfg(feature = "axum")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::body::Body;
use axum::extract::Request;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Extension, Json, Router};
#[cfg(feature = "tower-sessions")]
use http::header;
use http::{HeaderMap, StatusCode};
use http_body_util::BodyExt;
use inertia::axum::InertiaLayer;
use inertia::testing::AssertablePage;
use inertia::{Config, Inertia, ValidationErrors, props};
use tower::ServiceExt;
use tower_sessions::{MemoryStore, SessionManagerLayer};

fn app(config: Config) -> Router {
    Router::new()
        .route(
            "/",
            get(|inertia: Inertia| async move { inertia.render("Home", props! { "name" => "Taylor" }) }),
        )
        .route(
            "/form",
            get(|inertia: Inertia| async move { inertia.render("Form", ()) }).post(|inertia: Inertia| async move {
                inertia.flash("message", "Saved!");
                inertia.back_with_errors(
                    ValidationErrors::new()
                        .with("name", "Required.")
                        .with("name", "Too short."),
                )
            }),
        )
        .route(
            "/missing",
            get(|inertia: Inertia| async move { (StatusCode::NOT_FOUND, inertia.render("Error", ())) }),
        )
        .route(
            "/headed",
            get(|inertia: Inertia| async move { inertia.render("Home", ()).with_header("x-page", "home") }),
        )
        .route("/update", put(|| async { inertia::redirect("/") }))
        .route("/fragment", post(|| async { inertia::redirect("/page#section") }))
        .route("/empty", put(|| async { StatusCode::OK }))
        .route(
            "/away",
            get(|inertia: Inertia| async move { inertia.location("https://inertiajs.com") }),
        )
        .route("/json", get(|| async { Json(serde_json::json!({ "plain": true })) }))
        .route(
            "/login",
            get(|Extension(session): Extension<tower_sessions::Session>| async move {
                session.insert("user", 1).await.unwrap();
            }),
        )
        .route_layer(middleware::from_fn(share_user))
        .layer(InertiaLayer::new(config))
        .layer(SessionManagerLayer::new(MemoryStore::default()).with_secure(false))
}

/// Shares per-request data from a middleware, like an auth guard would.
async fn share_user(inertia: Inertia, request: Request, next: Next) -> Response {
    inertia.share("auth.user", "Taylor");
    next.run(request).await
}

fn config() -> Config {
    Config::new().version("1")
}

struct TestResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl TestResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }

    fn page(&self) -> AssertablePage {
        AssertablePage::from_body(&self.body)
    }

    #[cfg(feature = "tower-sessions")]
    fn session_cookie(&self) -> String {
        self.header("set-cookie")
            .and_then(|cookie| cookie.split(';').next())
            .expect("a session cookie")
            .to_owned()
    }
}

async fn send(app: &Router, request: http::Request<Body>) -> TestResponse {
    let response = app.clone().oneshot(request).await.unwrap().into_response();
    let (parts, body) = response.into_parts();

    TestResponse {
        status: parts.status,
        headers: parts.headers,
        body: String::from_utf8(body.collect().await.unwrap().to_bytes().to_vec()).unwrap(),
    }
}

fn visit(method: &str, uri: &str) -> http::request::Builder {
    http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost")
        .header("x-inertia", "true")
        .header("x-inertia-version", "1")
}

fn get_(uri: &str) -> http::Request<Body> {
    http::Request::get(uri)
        .header("host", "localhost")
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn first_visits_render_html() {
    let response = send(&app(config()), get_("/")).await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.header("content-type"), Some("text/html; charset=utf-8"));
    assert_eq!(response.header("vary"), Some("X-Inertia"));
    response
        .page()
        .component("Home")
        .equals("name", "Taylor")
        .equals("auth.user", "Taylor");
}

#[tokio::test]
async fn inertia_visits_return_json() {
    let response = send(&app(config()), visit("GET", "/").body(Body::empty()).unwrap()).await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.header("x-inertia"), Some("true"));
    assert_eq!(response.header("content-type"), Some("application/json"));
    response.page().component("Home").url("/").version("1");
}

#[tokio::test]
async fn pages_keep_their_headers_through_the_layer() {
    let app = app(config());

    assert_eq!(send(&app, get_("/headed")).await.header("x-page"), Some("home"));
    let visit = send(&app, visit("GET", "/headed").body(Body::empty()).unwrap()).await;
    assert_eq!(visit.header("x-page"), Some("home"));
    assert_eq!(visit.header("x-inertia"), Some("true"));
}

#[tokio::test]
async fn status_codes_set_around_a_render_are_kept() {
    let response = send(&app(config()), get_("/missing")).await;

    assert_eq!(response.status, StatusCode::NOT_FOUND);
    response.page().component("Error");
}

#[tokio::test]
async fn prop_callbacks_can_flash_to_their_http_response() {
    let app = Router::new()
        .route(
            "/",
            get(|inertia: Inertia| async move {
                let callback = inertia.clone();
                inertia.render(
                    "Home",
                    props! {
                        "name" => inertia::lazy(move || async move {
                            callback.flash("message", "Profile refreshed");
                            "Taylor"
                        }),
                    },
                )
            }),
        )
        .layer(InertiaLayer::new(config()));

    send(&app, visit("GET", "/").body(Body::empty()).unwrap())
        .await
        .page()
        .equals("name", "Taylor")
        .flash("message", "Profile refreshed");
}

#[tokio::test]
async fn outdated_clients_reload_the_page() {
    let request = http::Request::get("/?a=1")
        .header("host", "localhost")
        .header("x-inertia", "true")
        .header("x-inertia-version", "old")
        .body(Body::empty())
        .unwrap();
    let response = send(&app(config()), request).await;

    assert_eq!(response.status, StatusCode::CONFLICT);
    assert_eq!(response.header("x-inertia-location"), Some("http://localhost/?a=1"));
}

#[tokio::test]
async fn computed_versions_are_computed_once_per_request() {
    let computed = Arc::new(AtomicUsize::new(0));
    let config = Config::new().version_with({
        let computed = Arc::clone(&computed);
        move || {
            computed.fetch_add(1, Ordering::Relaxed);
            "1".to_owned()
        }
    });

    let response = send(&app(config), visit("GET", "/").body(Body::empty()).unwrap()).await;

    response.page().version("1");
    assert_eq!(computed.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn redirects_follow_the_protocol() {
    let app = app(config());

    let response = send(&app, visit("PUT", "/update").body(Body::empty()).unwrap()).await;
    assert_eq!(response.status, StatusCode::SEE_OTHER);
    assert_eq!(response.header("location"), Some("/"));

    let response = send(&app, http::Request::put("/update").body(Body::empty()).unwrap()).await;
    assert_eq!(response.status, StatusCode::FOUND, "plain requests are left alone");

    let response = send(&app, visit("POST", "/fragment").body(Body::empty()).unwrap()).await;
    assert_eq!(response.status, StatusCode::CONFLICT);
    assert_eq!(response.header("x-inertia-redirect"), Some("/page#section"));

    let response = send(
        &app,
        visit("PUT", "/empty")
            .header("referer", "/form")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status, StatusCode::SEE_OTHER);
    assert_eq!(response.header("location"), Some("/form"));

    let response = send(&app, visit("GET", "/away").body(Body::empty()).unwrap()).await;
    assert_eq!(response.status, StatusCode::CONFLICT);
    assert_eq!(response.header("x-inertia-location"), Some("https://inertiajs.com"));
}

#[tokio::test]
async fn other_responses_pass_through() {
    let response = send(&app(config()), visit("GET", "/json").body(Body::empty()).unwrap()).await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.body, r#"{"plain":true}"#);
    assert_eq!(response.header("vary"), Some("X-Inertia"));
}

#[cfg(feature = "tower-sessions")]
#[tokio::test]
async fn flash_data_and_errors_survive_a_redirect() {
    let app = app(config());

    let response = send(
        &app,
        visit("POST", "/form")
            .header("referer", "/form")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status, StatusCode::FOUND);
    let cookie = response.session_cookie();

    let response = send(
        &app,
        visit("GET", "/form")
            .header(header::COOKIE, &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    response
        .page()
        .equals("errors", serde_json::json!({ "name": "Required." }))
        .flash("message", "Saved!");

    let response = send(
        &app,
        visit("GET", "/form")
            .header(header::COOKIE, &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    response.page().equals("errors", serde_json::json!({}));
    assert!(response.page().page().flash.is_empty());
}

#[cfg(feature = "tower-sessions")]
#[tokio::test]
async fn renders_without_flash_data_leave_the_session_alone() {
    let app = app(config());
    let cookie = send(&app, get_("/login")).await.session_cookie();

    // The session layer only saves a modified session, and sets a cookie
    // when it does, so a read-only render can't overwrite another
    // request's changes.
    let response = send(
        &app,
        visit("GET", "/")
            .header(header::COOKIE, &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.header("set-cookie"), None);
}

#[cfg(feature = "tower-sessions")]
#[tokio::test]
async fn errors_are_scoped_to_the_requested_error_bag() {
    let app = app(config().with_all_errors(true));

    let response = send(
        &app,
        visit("POST", "/form")
            .header("referer", "/form")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let cookie = response.session_cookie();

    let request = visit("GET", "/form")
        .header(header::COOKIE, &cookie)
        .header("x-inertia-error-bag", "createUser");
    let response = send(&app, request.body(Body::empty()).unwrap()).await;

    response
        .page()
        .equals("errors.createUser.name", ["Required.", "Too short."]);
}

#[tokio::test]
async fn the_extractor_requires_the_layer() {
    let app = Router::new().route("/", get(|inertia: Inertia| async move { inertia.render("Home", ()) }));

    assert_eq!(send(&app, get_("/")).await.status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn renders_returned_without_the_layer_are_logged() {
    let errors = Arc::new(AtomicUsize::new(0));
    let _guard = tracing::subscriber::set_default(CountErrors(Arc::clone(&errors)));

    send(&app(config()), get_("/")).await;
    assert_eq!(errors.load(Ordering::Relaxed), 0, "the layer rendered it");

    let request = inertia::Request::new(http::Method::GET, &"/".parse().unwrap(), HeaderMap::new());
    drop(Inertia::new(config(), request).render("Home", ()).into_response());
    assert_eq!(errors.load(Ordering::Relaxed), 1);
}

/// Counts the `error` events Inertia logs.
struct CountErrors(Arc<AtomicUsize>);

impl tracing::Subscriber for CountErrors {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let metadata = event.metadata();

        if *metadata.level() == tracing::Level::ERROR && metadata.target().starts_with("inertia") {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

fn failing_page(inertia: Inertia) -> inertia::Response {
    inertia.render(
        "Home",
        props! {
            "stats" => inertia::try_lazy(|| async { Err::<u32, _>(std::io::Error::other("The stats are down")) }),
        },
    )
}

#[tokio::test]
async fn a_failing_prop_is_a_server_error() {
    let app = Router::new()
        .route("/", get(|inertia: Inertia| async move { failing_page(inertia) }))
        .layer(InertiaLayer::new(config()));

    assert_eq!(send(&app, get_("/")).await.status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn handle_with_responds_to_a_failing_prop() {
    let response = InertiaLayer::new(config())
        .handle_with(
            get_("/"),
            |request| async move {
                let inertia = request.extensions().get::<Inertia>().cloned().unwrap();
                let page = failing_page(inertia.clone()).into_response();
                assert!(inertia::axum::is_render(&page));

                Ok::<_, std::convert::Infallible>(page)
            },
            |error| {
                let source = error.into_inner();
                assert!(source.downcast_ref::<std::io::Error>().is_some());

                (StatusCode::SERVICE_UNAVAILABLE, source.to_string()).into_response()
            },
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(body, "The stats are down");
}
