# Inertia.js for Rust

Build modern single-page apps with React, Vue or Svelte, routed and rendered by your Rust server. No client-side router, no API to build and maintain: handlers return a page component and its props, and [Inertia.js](https://inertiajs.com) does the rest.

```rust
async fn show(inertia: Inertia, Path(id): Path<u64>, State(db): State<Db>) -> inertia::Response {
    let user = db.find_user(id).await;

    inertia.render("Users/Show", props! {
        "user" => user,
        "activity" => inertia::defer(move || async move { db.activity_for(id).await }),
    })
}
```

This crate implements the server side of the [Inertia v3 protocol](https://inertiajs.com/docs/v3/core-concepts/the-protocol), ported from the official [Laravel adapter](https://github.com/inertiajs/inertia-laravel). Its core is framework-agnostic; an adapter for [Axum](https://github.com/tokio-rs/axum) ships with it.

- [Installation](#installation)
- [Quick start](#quick-start)
- [Responses](#responses)
- [Props](#props)
- [Shared data](#shared-data)
- [Forms, validation and flash data](#forms-validation-and-flash-data)
- [Redirects](#redirects)
- [History encryption](#history-encryption)
- [The root view and Vite](#the-root-view-and-vite)
- [Server-side rendering](#server-side-rendering)
- [Testing](#testing)
- [Architecture and writing an adapter](#architecture-and-writing-an-adapter)
- [Feature flags](#feature-flags)
- [Coming from Laravel](#coming-from-laravel)

## Installation

```toml
[dependencies]
inertia-omega = { path = "../inertia-omega" }
```

The crate's library name is `inertia`, so Rust imports use `inertia::…`. The sibling path above assumes the current [workspace layout](https://github.com/laravel/omega-workspace#setup).

The default features include the Axum adapter, `tower-sessions` support and server-side rendering. For the framework-agnostic core alone, use `default-features = false`.

On the client, follow the Inertia [client-side setup](https://inertiajs.com/docs/v3/installation/client-side-setup) for React, Vue or Svelte. The [demo app](../axum-inertia-app) is a complete example with React, TypeScript, Tailwind, Vite and SSR.

## Quick start

```rust
use axum::{Router, routing::get};
use inertia::axum::InertiaLayer;
use inertia::{Config, Inertia, props};
use tower_sessions::{MemoryStore, SessionManagerLayer};

async fn home(inertia: Inertia) -> inertia::Response {
    inertia.render("Home", props! { "greeting" => "Hello from Rust" })
}

#[tokio::main]
async fn main() {
    let config = Config::new()
        .version("1")
        .root_view(|view: &inertia::View<'_>| {
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
        // The session layer goes outside of the Inertia layer.
        .layer(SessionManagerLayer::new(MemoryStore::default()).with_secure(false));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
```

This example assumes your frontend bundle is served at `/build/app.js`. It shows the server wiring; see the [demo app](../axum-inertia-app) for asset serving and frontend setup. `.with_secure(false)` allows session cookies on local HTTP; use secure cookies in HTTPS deployments.

The [`Inertia`](src/inertia.rs) extractor is the handle for the current request. `render` returns an [`inertia::Response`](src/response.rs) right away, so handlers don't `.await` it. Once the handler returns, the layer resolves the props and responds with an HTML document on a first visit, or the page as JSON on an Inertia visit, much like a Laravel `Responsable`.

## Responses

`render` takes a component name and its props: either [`Props`](src/props/mod.rs), usually built with the `props!` macro, or any `Serialize` struct.

```rust
#[derive(Serialize)]
struct ShowProps {
    user: User,
    can_edit: bool,
}

async fn show(inertia: Inertia) -> inertia::Response {
    inertia
        .render("Users/Show", ShowProps { user, can_edit: true })
        .with("title", "Profile")         // add a prop
        .with_view_data("meta", "...")    // data for the root view only
        .with_header("cache-control", "no-store")  // sent with the page, as JSON or HTML
}
```

Status codes and headers set around a render are kept, so an error page is just a tuple:

```rust
async fn not_found(inertia: Inertia) -> impl IntoResponse {
    (StatusCode::NOT_FOUND, inertia.render("Error", props! { "status" => 404 }))
}
```

## Props

Any `Serialize` value is a prop. Keys may use dot notation to build nested props, and props can be nested with `props!` as well:

```rust
props! {
    "user" => user,                       // a struct
    "auth.user.permissions" => ["edit"],  // nested with dot notation
    "settings" => props! {                // nested props, with behavior of their own
        "theme" => "dark",
        "billing" => inertia::optional(|| async { load_billing().await }),
    },
}
```

Partial reloads (`router.reload({ only: ['user.name'] })`) only resolve the props they ask for, matching nested paths in both directions.

The functions at the crate root create props with special behavior, which builder methods refine further:

| Prop | Behavior |
| --- | --- |
| `inertia::lazy(\|\| async { .. })` | Computed only when the prop is part of the response. |
| `inertia::optional(..)` | Only sent when a partial reload asks for it. |
| `inertia::defer(..)` | Left out of the first render; the client fetches it right after. `.group("name")` loads deferred props in parallel groups. |
| `inertia::always(value)` | Sent whole with every response, even partial reloads that didn't ask for it. |
| `inertia::merge(value)` | Appended to its client-side value. `.prepend()`, `.append_at("data")`, `.prepend_at("data")`, `.match_on("id")`. |
| `inertia::deep_merge(value)` | Deep merged with its client-side value. |
| `inertia::once(..)` | Resolved once and remembered by the client. `.once_as("key")`, `.until(duration)`, `.fresh()`. |
| `inertia::scroll(paginator)` | An infinite scroll page, for `<InfiniteScroll>`. `inertia::scroll_with(\|\| async { .. })` loads it lazily. |
| `inertia::try_lazy(..)` | A fallible callback. Errors fail the response, unless the prop is `.rescue()`d. `Response::into_page` returns the error as a `PropError`, whose `get_ref()` and `into_inner()` give back the callback's own error. |

The behaviors compose:

```rust
props! {
    "posts" => inertia::defer(|| async { load_posts().await }).group("feed").merge().match_on("id"),
    "plans" => inertia::once(|| async { load_plans().await }).until(Duration::from_secs(3600)),
    "stats" => inertia::try_lazy(|| async { stats_api().await }).deferred().rescue(),
}
```

Callbacks are closures returning futures (`FnOnce`), so they can move in owned data such as a database pool. **Callbacks run concurrently**, nested ones included, so independent lookups can overlap rather than run one after another.

For infinite scroll, `Paginator` paginates a collection and describes its pages; implement `ProvidesScrollMetadata` for your own paginated types:

```rust
"users" => inertia::scroll_with(move || async move {
    Paginator::from_items(db.users().await, 15, page)
}),
```

## Shared data

Share data with every page from the config. The callback runs on each render, so make anything expensive lazy:

```rust
Config::new().share(|_request| props! {
    "appName" => "Acme",
    "announcement" => inertia::lazy(|| async { latest_announcement().await }),
})
```

For data that depends on the request, such as the signed-in user, share it from a middleware. The `Inertia` handle is shared by everything that handles the request:

```rust
use axum::extract::Request;

async fn share_user(inertia: Inertia, user: CurrentUser, request: Request, next: Next) -> Response {
    inertia.share("auth.user", user);
    next.run(request).await
}

let app = Router::new()
    .route("/", get(home))
    .layer(middleware::from_fn(share_user))  // inside of the Inertia layer
    .layer(InertiaLayer::new(config));
```

Validation errors are shared automatically as `errors`. Shared prop keys are listed in the page's `sharedProps`.

## Forms, validation and flash data

The Inertia client sends form data as JSON, or multipart data for uploads. Validate it, and on failure redirect back with the errors. They're shared with the next page in its `errors` prop:

```rust
async fn store(inertia: Inertia, Json(form): Json<NewUser>) -> HttpResponse {
    let mut errors = ValidationErrors::new();

    if form.name.trim().is_empty() {
        errors.add("name", "The name field is required.");
    }

    if !errors.is_empty() {
        return inertia.back_with_errors(errors);
    }

    create_user(form).await;
    inertia.flash("toast", "User created!");

    inertia::redirect("/users")
}
```

- **Error bags.** Forms with an `errorBag` get their errors scoped under it (`errors.createUser.name`). `Config::with_all_errors(true)` shares every message per field instead of the first.
- **Validation crates.** With the `validator` or `garde` features, their errors convert into `ValidationErrors` with `.into()`.
- **Flash data** is delivered in the page's `flash` field, not its props, so it isn't kept in the browser history.

Like Laravel's session, `flash`, `with_errors`, `clear_history` and `preserve_fragment` are queued during the request, written to the session when it ends, and delivered to the next page render. That can be a render in the same request. A render that fails leaves them for the one after it.

Flash data queued by prop callbacks also reaches the page being rendered. Values set with `inertia::Response::flash` take precedence over the request's flash data.

## Redirects

| Call | Response |
| --- | --- |
| `inertia::redirect("/users")` | `302`. After `PUT`, `PATCH` and `DELETE` the layer makes it a `303`, so the browser follows it with a `GET`. |
| `inertia.back()` | A redirect to the `Referer`. |
| `inertia.location("https://..")` | A full page visit: `409` with `X-Inertia-Location` for Inertia visits. Use it for external URLs. |

The layer also handles the protocol's edge cases:

- A redirect to a URL with a `#fragment` becomes a `409` with `X-Inertia-Redirect`, since `fetch` drops fragments.
- An empty `200` response to an Inertia visit becomes a redirect back.
- A client running outdated assets (a different `version`) gets a `409` that makes it reload the page.

## History encryption

```rust
inertia.render("Billing", props).encrypt_history(true)  // or Config::encrypt_history(true) for every page
inertia.clear_history();                                  // e.g. on logout: encrypted pages can't be restored
```

## The root view and Vite

The root view renders the HTML document of a first visit. It is any `Fn(&View) -> String`, or a type implementing `RootView`. A `View` holds:

- `head`: the SSR `<head>` tags,
- `body`: the page data `<script>` and the app element,
- `page`: the page object,
- `data`: view data,
- `ssr`: whether the page was server-side rendered.

The [`laravel-omega-vite`](../laravel-omega-vite) crate's `Vite` renders asset tags like Laravel's `@vite`. It's a separate dependency, `laravel-omega-vite = { path = "../laravel-omega-vite" }`, used as `vite::Vite`:

- **In development** the Vite dev server writes its URL to a hot file (`public/hot`), and tags point at the dev server, with `Vite::react_refresh()` providing React Fast Refresh's preamble.
- **In production** tags come from the build manifest, and its hash makes a good asset version.

The manifest is parsed once and cached. Each render checks for the hot file and the manifest's modified time, so starting the dev server or a new build is picked up without a restart. In production, where neither changes without a deploy, `Vite::new().watch(false)` checks each once instead, and so does `HttpGateway::watch(false)` for its hot file and bundle, which it otherwise checks at most once a second.

```rust
let vite = Vite::new();

Config::new()
    .version_with({
        let vite = vite.clone();
        move || vite.manifest_hash().unwrap_or_default()
    })
    .root_view(move |view: &View<'_>| format!(
        "<!DOCTYPE html><html><head>{refresh}{assets}{head}</head><body>{body}</body></html>",
        refresh = vite.react_refresh(),
        assets = vite.tags(["resources/js/app.tsx", &format!("resources/js/pages/{}.tsx", view.page.component)]),
        head = view.head,
        body = view.body,
    ))
```

Vite has no hot file of its own. The demo app's [`vite.config.ts`](../axum-inertia-app/vite.config.ts) adds a ten-line plugin that writes one, like `laravel-vite-plugin` does.

## Server-side rendering

```rust
use inertia::ssr::HttpGateway;

Config::new().ssr(
    HttpGateway::new()
        .hot_file("public/hot")             // dev: render through Vite's /__inertia_ssr endpoint
        .bundle("bootstrap/ssr/app.js")     // prod: use :13714/render once the bundle is built
        .except(["admin/*"]),
)
```

On first visits the page is rendered by the SSR server, and the client hydrates it. If the server is down or rendering fails, the page is rendered on the client instead. A slow SSR server can still delay the response until its timeout. Implement `ssr::Gateway` to render pages another way.

## Testing

`AssertablePage` reads a page from a response body (an HTML document or JSON), with assertions modeled on Laravel's `assertInertia`:

```rust
use inertia::testing::AssertablePage;

AssertablePage::from_body(&body)
    .component("Users/Index")
    .has_count("users", 3)
    .equals("users.0.name", "Taylor")
    .missing("users.0.password")
    .deferred("default", &["stats"])
    .flash("toast", "Saved!");
```

The core can also be tested without a web framework. Build an `Inertia` handle from a `Request` and an `ArraySession`, then resolve a render with `Response::into_page`. See [`tests/core.rs`](tests/core.rs).

## Architecture and writing an adapter

```text
src/
├── config.rs       Config: version, root view, shared props, SSR
├── inertia.rs      Inertia: the per-request handle (render, share, flash, errors, redirects)
├── request.rs      Request: the protocol headers, parsed once
├── props/          Props, Prop and its behaviors, and the props resolver
├── response.rs     Response: a pending render, resolved into an HTTP response
├── page.rs         Page: the typed page object
├── protocol.rs     The rules applied around each handler: before() and after()
├── session/        The Session trait, ArraySession, and tower-sessions support
├── ssr.rs          The Gateway trait and HttpGateway
├── view.rs         The RootView trait
├── testing.rs      AssertablePage
└── axum/           The Axum adapter: InertiaLayer, the extractor, IntoResponse
```

The core depends on `http`, `serde` and utility crates. Optional features add HTTP SSR, session stores and framework or validation integrations. An adapter for another framework is a thin translation layer:

```rust
// 1. Parse the request.
let request = inertia::Request::new(method, &uri, headers);

// 2. Outdated clients reload the page instead of running the handler.
if let Some(response) = inertia::protocol::before(&request, &config) {
    return response;
}

// 3. Make the handle available to handlers, with the framework's session.
let inertia = Inertia::with_session(config.clone(), request, session);

// 4. Run the handler. If it returned an inertia::Response, resolve it.
//    `try_into_http` returns a prop's failure instead of a plain `500`,
//    for the framework to render as it renders any error.
let response = page.into_http().await;

// 5. Write queued flash data and errors to the session.
inertia.commit().await;

// 6. Apply the redirect rules, which may replace the response.
let (mut parts, body) = response.into_parts();
if let Some(replacement) = inertia::protocol::after(inertia.request(), &mut parts, body.is_empty()) {
    return replacement;
}
```

Implement the `Session` trait (`get`, `put`, `pull`) for the framework's session. See [`src/axum`](src/axum) for the complete Axum adapter.

## Feature flags

| Feature | Default | |
| --- | --- | --- |
| `axum` | ✓ | The Axum adapter |
| `tower-sessions` | ✓ | `Session` for `tower_sessions::Session` |
| `ssr` | ✓ | `HttpGateway`, which uses `reqwest` |
| `validator` | | `From<validator::ValidationErrors>` for `ValidationErrors` |
| `garde` | | `From<garde::Report>` for `ValidationErrors` |

## Coming from Laravel

| Laravel | Rust |
| --- | --- |
| `Inertia::render('Users/Index', [...])` | `inertia.render("Users/Index", props! { .. })` |
| `Inertia::optional(fn () => ..)` | `inertia::optional(\|\| async { .. })` |
| `Inertia::defer(fn () => ..)->group('x')` | `inertia::defer(\|\| async { .. }).group("x")` |
| `Inertia::merge(..)->matchOn('id')` | `inertia::merge(..).match_on("id")` |
| `Inertia::deepMerge(..)` / `Inertia::always(..)` | `inertia::deep_merge(..)` / `inertia::always(..)` |
| `Inertia::once(fn () => ..)->until(..)` | `inertia::once(\|\| async { .. }).until(..)` |
| `Inertia::scroll(User::paginate())` | `inertia::scroll(Paginator::from_items(..))` |
| `HandleInertiaRequests::share()` | `Config::share` or `inertia.share(..)` in a middleware |
| `HandleInertiaRequests::version()` | `Config::version` / `Config::version_with` |
| `back()->withErrors($errors)` | `inertia.back_with_errors(errors)` |
| `Inertia::flash('key', $value)` | `inertia.flash("key", value)` |
| `Inertia::location($url)` | `inertia.location(url)` |
| `Inertia::clearHistory()` / `encryptHistory()` | `inertia.clear_history()` / `.encrypt_history(true)` |
| `$response->assertInertia(fn ($page) => ..)` | `AssertablePage::from_body(&body)..` |

Not ported yet: Inertia DevTools, `ProvidesInertiaProperties`, component name transformers, the `ensure_pages_exist` check, and Precognition.

## Acknowledgements

The props resolver and protocol rules are ports of [inertiajs/inertia-laravel](https://github.com/inertiajs/inertia-laravel). The framework-agnostic core with adapters, and handlers that return a render without awaiting it, were inspired by [Veer](https://github.com/climactic/veer).

## Tests

```bash
cargo test --all-features
```
