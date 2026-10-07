//! The framework-agnostic core, exercised without a web framework.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use http::{HeaderMap, HeaderName, Method};
use inertia::session::ArraySession;
use inertia::testing::AssertablePage;
use inertia::{Config, Inertia, Page, Paginator, Props, Request, ValidationErrors, props};
use serde::Serialize;
use serde_json::json;
use tokio::sync::Barrier;

fn config() -> Config {
    Config::new()
        .version("1")
        .share(|_| props! { "appName" => "Demo", "auth.check" => true })
}

fn request(method: Method, headers: &[(&'static str, &'static str)]) -> Request {
    let headers: HeaderMap = headers
        .iter()
        .map(|(name, value)| (HeaderName::from_static(name), value.parse().unwrap()))
        .collect();

    Request::new(method, &"/users?page=1".parse().unwrap(), headers)
}

/// An Inertia visit with the given extra headers.
fn visit(headers: &[(&'static str, &'static str)]) -> Inertia {
    let mut all = vec![("x-inertia", "true"), ("x-inertia-version", "1")];
    all.extend_from_slice(headers);

    Inertia::new(config(), request(Method::GET, &all))
}

/// A partial reload of `Users` with the given extra headers.
fn reload(headers: &[(&'static str, &'static str)]) -> Inertia {
    let mut all = vec![("x-inertia-partial-component", "Users")];
    all.extend_from_slice(headers);

    visit(&all)
}

async fn resolve(response: inertia::Response) -> Page {
    response.into_page().await.unwrap()
}

fn users_props() -> Props {
    props! {
        "users" => ["Taylor", "Jess"],
        "auth.user.id" => 1,
        "auth.user.name" => "Taylor",
        "lazy" => inertia::lazy(|| async { "computed" }),
        "nested" => props! { "a" => 1, "b" => inertia::optional(|| async { 2 }) },
        "literal" => json!({ "x": 1, "y": { "z": 2 } }),
        "optional" => inertia::optional(|| async { "optional" }),
        "always" => inertia::always("always"),
    }
}

#[tokio::test]
async fn resolves_shared_and_page_props() {
    let page = resolve(visit(&[]).render("Users", users_props())).await;

    assert_eq!(
        serde_json::to_value(&page.props).unwrap(),
        json!({
            "errors": {},
            "appName": "Demo",
            "auth": { "check": true, "user": { "id": 1, "name": "Taylor" } },
            "users": ["Taylor", "Jess"],
            "lazy": "computed",
            "nested": { "a": 1 },
            "literal": { "x": 1, "y": { "z": 2 } },
            "always": "always",
        })
    );
    assert_eq!(page.url, "/users?page=1");
    assert_eq!(page.version, "1");
    assert_eq!(page.metadata.shared_props, ["errors", "appName", "auth"]);
}

#[tokio::test]
async fn renders_serializable_structs() {
    #[derive(Serialize)]
    struct UsersProps {
        users: Vec<&'static str>,
    }

    let page = resolve(
        visit(&[])
            .render("Users", UsersProps { users: vec!["Taylor"] })
            .with("count", 1),
    )
    .await;

    assert_eq!(page.props["users"], json!(["Taylor"]));
    assert_eq!(page.props["count"], 1);

    let error = visit(&[]).render("Users", [1, 2]).into_page().await.unwrap_err();
    assert!(error.to_string().contains("must serialize to a JSON object"));
}

#[tokio::test]
async fn partial_reloads_only_include_the_requested_props() {
    let page = resolve(reload(&[("x-inertia-partial-data", "users,optional")]).render("Users", users_props())).await;
    assert_eq!(
        serde_json::to_value(&page.props).unwrap(),
        json!({ "errors": {}, "users": ["Taylor", "Jess"], "optional": "optional", "always": "always" })
    );

    let page = resolve(
        reload(&[("x-inertia-partial-data", "auth.user.name,nested.b,literal.y")]).render("Users", users_props()),
    )
    .await;
    assert_eq!(
        serde_json::to_value(&page.props).unwrap(),
        json!({
            "errors": {},
            "auth": { "user": { "name": "Taylor" } },
            "nested": { "b": 2 },
            "literal": { "y": { "z": 2 } },
            "always": "always",
        })
    );
}

#[tokio::test]
async fn always_props_keep_their_children_on_partial_reloads() {
    let inertia = reload(&[("x-inertia-partial-data", "users")]);
    inertia.with_errors(ValidationErrors::new().with("name", "A name is required."));
    let page = resolve(inertia.render(
        "Users",
        props! {
            "users" => ["Taylor"],
            "profile" => inertia::always(json!({ "user": { "id": 1 } })),
            "settings" => inertia::always(props! { "theme" => "dark" }),
        },
    ))
    .await;

    assert_eq!(page.props["errors"], json!({ "name": "A name is required." }));
    assert_eq!(page.props["profile"], json!({ "user": { "id": 1 } }));
    assert_eq!(page.props["settings"], json!({ "theme": "dark" }));
}

#[test]
fn serializing_a_shared_prop_can_share_another_prop() {
    struct User(Inertia);

    impl Serialize for User {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            self.0.share("signed_in", true);
            serializer.serialize_str("Taylor")
        }
    }

    let (sender, receiver) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        let inertia = visit(&[]);
        inertia.share("user", User(inertia.clone()));
        sender.send(inertia).unwrap();
    });
    let inertia = receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("sharing must not hold the pending-data lock while serializing");
    thread.join().unwrap();

    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let page = runtime.block_on(resolve(inertia.render("Users", props! {})));
    assert_eq!(page.props["user"], json!("Taylor"));
    assert_eq!(page.props["signed_in"], json!(true));
}

#[tokio::test]
async fn partial_reloads_exclude_the_excepted_props() {
    let inertia = reload(&[("x-inertia-partial-except", "auth,literal.x,errors")]);
    inertia.with_errors(ValidationErrors::new().with("name", "Required."));

    let page = resolve(inertia.render("Users", users_props())).await;
    let page = AssertablePage::from_body(&serde_json::to_string(&page).unwrap());

    page.missing("auth")
        .equals("errors", json!({ "name": "Required." }))
        .equals("literal", json!({ "y": { "z": 2 } }))
        .equals("optional", "optional");
}

#[tokio::test]
async fn partial_reloads_send_always_props_whole() {
    let props = || {
        props! {
            "users" => ["Taylor"],
            "object" => inertia::always(json!({ "name": "Taylor", "nested": { "x": 1 } })),
            "nestedProps" => inertia::always(props! { "a" => 1, "b" => props! { "c" => 2 } }),
        }
    };

    for header in [
        ("x-inertia-partial-data", "users"),
        ("x-inertia-partial-except", "users,object.name,nestedProps.b"),
    ] {
        let inertia = reload(&[header]);
        inertia.with_errors(ValidationErrors::new().with("name", "Required."));

        let page = resolve(inertia.render("Users", props())).await;

        assert_eq!(page.props["errors"], json!({ "name": "Required." }), "{header:?}");
        assert_eq!(
            page.props["object"],
            json!({ "name": "Taylor", "nested": { "x": 1 } }),
            "{header:?}"
        );
        assert_eq!(
            page.props["nestedProps"],
            json!({ "a": 1, "b": { "c": 2 } }),
            "{header:?}"
        );
    }

    let inertia = reload(&[
        ("x-inertia-partial-data", "users"),
        ("x-inertia-error-bag", "createUser"),
    ]);
    inertia.with_errors(ValidationErrors::new().with("name", "Required."));

    let page = resolve(inertia.render("Users", props())).await;
    assert_eq!(page.props["errors"], json!({ "createUser": { "name": "Required." } }));
}

#[tokio::test]
async fn partial_reloads_of_another_component_are_full_visits() {
    let page = resolve(reload(&[("x-inertia-partial-data", "users")]).render("Dashboard", users_props())).await;

    assert_eq!(page.props["appName"], "Demo");
    assert!(page.props.get("optional").is_none());
}

#[tokio::test]
async fn dot_keys_set_inside_callbacks_without_running_them_early() {
    let loaded = Arc::new(AtomicBool::new(false));
    let props = || {
        let loaded = Arc::clone(&loaded);

        props! {
            "users" => ["Taylor"],
            "auth" => inertia::lazy(move || async move {
                loaded.store(true, Ordering::SeqCst);
                json!({ "user": { "name": "Taylor" } })
            }),
            "auth.user.id" => 1,
            "auth.permissions" => inertia::lazy(|| async { ["edit"] }),
        }
    };

    for header in [
        ("x-inertia-partial-data", "users"),
        ("x-inertia-partial-except", "auth"),
    ] {
        let page = resolve(reload(&[header]).render("Users", props())).await;

        assert!(page.props.get("auth").is_none(), "{header:?}");
        assert!(!loaded.load(Ordering::SeqCst), "{header:?}");
    }

    let page = resolve(visit(&[]).render("Users", props())).await;
    assert_eq!(
        page.props["auth"],
        json!({ "user": { "name": "Taylor", "id": 1 }, "check": true, "permissions": ["edit"] })
    );
}

#[tokio::test]
async fn dot_keys_replace_rescued_callbacks() {
    let page = resolve(visit(&[]).render(
        "Users",
        props! {
            "stats" => inertia::try_lazy(|| async { Err::<u8, _>("boom") }).rescue(),
            "stats.total" => 1,
        },
    ))
    .await;

    assert_eq!(page.props["stats"], json!({ "total": 1 }));
}

#[tokio::test]
async fn deferred_props_are_announced_then_loaded_by_group() {
    let props = || {
        props! {
            "users" => inertia::defer(|| async { ["Taylor"] }),
            "stats" => inertia::defer(|| async { 42 }).group("stats"),
            "broken" => inertia::try_lazy(|| async { Err::<u8, _>("boom") }).deferred().rescue(),
        }
    };

    let page = resolve(visit(&[]).render("Users", props())).await;
    assert!(page.props.get("users").is_none());
    assert_eq!(page.metadata.deferred_props["default"], ["users", "broken"]);
    assert_eq!(page.metadata.deferred_props["stats"], ["stats"]);

    let page = resolve(reload(&[("x-inertia-partial-data", "users,broken")]).render("Users", props())).await;
    assert_eq!(
        serde_json::to_value(&page.props).unwrap(),
        json!({ "errors": {}, "users": ["Taylor"] })
    );
    assert_eq!(page.metadata.rescued_props, ["broken"]);
    assert!(page.metadata.deferred_props.is_empty());
}

#[tokio::test]
async fn unrescued_failures_fail_the_render() {
    let response = visit(&[]).render(
        "Users",
        props! { "broken" => inertia::try_lazy(|| async { Err::<u8, _>("boom") }) },
    );

    assert_eq!(response.into_page().await.unwrap_err().to_string(), "boom");
}

#[tokio::test]
async fn merge_props_describe_how_to_merge() {
    let props = || {
        props! {
            "append" => inertia::merge([1, 2]),
            "prepend" => inertia::merge([1]).prepend(),
            "deep" => inertia::deep_merge(json!({ "a": 1 })),
            "matched" => inertia::merge(json!([{ "id": 1 }])).match_on("id"),
            "paths" => inertia::merge(json!({ "data": [], "meta": [] })).append_at("data").match_on("data.id").prepend_at("meta"),
            "deferredMerge" => inertia::defer(|| async { [1] }).merge(),
        }
    };

    let metadata = resolve(visit(&[]).render("Users", props())).await.metadata;
    assert_eq!(
        metadata.merge_props,
        ["append", "matched", "paths.data", "deferredMerge"]
    );
    assert_eq!(metadata.prepend_props, ["prepend", "paths.meta"]);
    assert_eq!(metadata.deep_merge_props, ["deep"]);
    assert_eq!(metadata.match_props_on, ["matched.id", "paths.data.id"]);

    let reset = reload(&[
        ("x-inertia-partial-data", "append,prepend"),
        ("x-inertia-reset", "prepend"),
    ]);
    let metadata = resolve(reset.render("Users", props())).await.metadata;
    assert_eq!(metadata.merge_props, ["append"]);
    assert!(metadata.prepend_props.is_empty());
}

#[tokio::test]
async fn once_props_are_skipped_when_the_client_has_them() {
    let props = || {
        props! {
            "plans" => inertia::once(|| async { ["basic"] }),
            "countries" => inertia::once(|| async { ["NL"] }).once_as("geo").until(Duration::from_secs(60)),
            "fresh" => inertia::once(|| async { 1 }).fresh(),
        }
    };

    let page = resolve(visit(&[]).render("Users", props())).await;
    assert_eq!(page.props["plans"], json!(["basic"]));
    assert_eq!(page.metadata.once_props["plans"].prop, "plans");
    assert_eq!(page.metadata.once_props["plans"].expires_at, None);
    assert_eq!(page.metadata.once_props["geo"].prop, "countries");
    assert!(page.metadata.once_props["geo"].expires_at.is_some());

    let page = resolve(visit(&[("x-inertia-except-once-props", "plans,geo,fresh")]).render("Users", props())).await;
    assert!(page.props.get("plans").is_none());
    assert!(page.props.get("countries").is_none());
    assert_eq!(page.props["fresh"], 1);
    assert!(
        page.metadata.once_props.contains_key("plans"),
        "the client keeps remembering them"
    );
}

#[tokio::test]
async fn once_props_with_huge_ttls_saturate() {
    let page = resolve(visit(&[]).render(
        "Users",
        props! { "plans" => inertia::once(|| async { 1 }).until(Duration::MAX) },
    ))
    .await;

    assert_eq!(page.metadata.once_props["plans"].expires_at, Some(u64::MAX));
}

#[test]
fn prop_errors_are_transparent() {
    let error = inertia::PropError::new(std::io::Error::other("database down"));

    assert_eq!(error.to_string(), "database down");
    assert!(
        std::error::Error::source(&error).is_none(),
        "the message would appear twice"
    );
}

#[test]
fn prop_errors_give_access_to_the_wrapped_error() {
    let error = inertia::PropError::new(std::io::Error::new(std::io::ErrorKind::NotFound, "no such user"));

    let wrapped = error.get_ref().downcast_ref::<std::io::Error>().unwrap();
    assert_eq!(wrapped.kind(), std::io::ErrorKind::NotFound);

    let wrapped = error.into_inner().downcast::<std::io::Error>().unwrap();
    assert_eq!(wrapped.kind(), std::io::ErrorKind::NotFound);
}

#[tokio::test]
async fn failed_props_keep_their_error() {
    let props = props! {
        "user" => inertia::try_lazy(|| async {
            Err::<(), _>(std::io::Error::new(std::io::ErrorKind::NotFound, "no such user"))
        }),
    };

    let error = visit(&[]).render("Users", props).into_page().await.unwrap_err();

    assert!(error.get_ref().is::<std::io::Error>());
}

#[tokio::test]
async fn scroll_props_carry_pagination_metadata() {
    let props = || {
        props! {
            "users" => inertia::scroll(Paginator::from_items(1..=25, 10, 2)),
            "later" => inertia::scroll_with(|| async { Paginator::from_items(1..=5, 10, 1) }).deferred(),
        }
    };

    let page = resolve(visit(&[]).render("Users", props())).await;
    assert_eq!(
        page.props["users"]["data"],
        json!([11, 12, 13, 14, 15, 16, 17, 18, 19, 20])
    );
    assert_eq!(
        serde_json::to_value(&page.metadata.scroll_props["users"]).unwrap(),
        json!({ "pageName": "page", "previousPage": 1, "nextPage": 3, "currentPage": 2, "reset": false })
    );
    assert_eq!(page.metadata.merge_props, ["users.data", "later"]);
    assert_eq!(page.metadata.deferred_props["default"], ["later"]);

    let prepend = reload(&[
        ("x-inertia-partial-data", "later"),
        ("x-inertia-infinite-scroll-merge-intent", "prepend"),
    ]);
    let page = resolve(prepend.render("Users", props())).await;
    assert_eq!(page.metadata.prepend_props, ["later.data"]);
    assert_eq!(page.metadata.scroll_props["later"].metadata.next_page, None);
}

#[test]
fn paginator_normalizes_a_zero_page() {
    let paginator = Paginator::new(vec!["Taylor"], 1, 15, 0);

    assert_eq!(paginator.current_page, 1);
    assert_eq!(paginator.from, Some(1));
    assert_eq!(paginator.to, Some(1));
}

#[tokio::test]
async fn sibling_callbacks_resolve_concurrently() {
    let barrier = Arc::new(Barrier::new(3));
    let wait = |value: u8| {
        let barrier = Arc::clone(&barrier);
        inertia::lazy(move || async move {
            barrier.wait().await;
            value
        })
    };

    let page = tokio::time::timeout(
        Duration::from_secs(5),
        resolve(visit(&[]).render("Users", props! { "a" => wait(1), "b" => wait(2), "c" => wait(3) })),
    )
    .await
    .expect("all sibling callbacks must run before any waits for the others");

    assert_eq!(page.props["a"], 1);
    assert_eq!(page.props["b"], 2);
    assert_eq!(page.props["c"], 3);
}

#[tokio::test]
async fn nested_callbacks_resolve_concurrently() {
    // Each callback waits for the others, so resolving them one after
    // another would never finish.
    let barrier = Arc::new(Barrier::new(3));
    let waiting = |value: u8| {
        let barrier = Arc::clone(&barrier);

        inertia::lazy(move || async move {
            barrier.wait().await;
            value
        })
    };

    let render = visit(&[]).render(
        "Users",
        props! {
            "auth.user" => waiting(1),
            "stats" => waiting(2),
            "nested" => props! { "deeper" => props! { "total" => waiting(3) } },
        },
    );
    let page = tokio::time::timeout(Duration::from_secs(5), resolve(render))
        .await
        .expect("the callbacks should run concurrently");

    assert_eq!(page.props["auth"]["user"], 1);
    assert_eq!(page.props["stats"], 2);
    assert_eq!(page.props["nested"], json!({ "deeper": { "total": 3 } }));
}

#[tokio::test]
async fn nested_metadata_is_collected_in_prop_order() {
    let props = props! {
        "a" => inertia::merge([1]),
        "nested" => props! {
            "b" => inertia::merge([2]),
            "deeper" => props! { "c" => inertia::defer(|| async { 3 }).merge() },
        },
        "d" => inertia::defer(|| async { 4 }).merge(),
        "e.f" => inertia::merge([5]),
    };

    let metadata = resolve(visit(&[]).render("Users", props)).await.metadata;

    assert_eq!(metadata.merge_props, ["a", "nested.b", "nested.deeper.c", "d", "e.f"]);
    assert_eq!(metadata.deferred_props["default"], ["nested.deeper.c", "d"]);
}

#[tokio::test]
async fn big_integers_can_be_preserved() {
    let page = resolve(
        visit(&[])
            .render("Users", props! { "id" => u64::MAX, "small" => 1 })
            .preserve_big_integers(true),
    )
    .await;

    assert_eq!(page.props["id"], json!({ "$bigint": "18446744073709551615" }));
    assert_eq!(page.props["small"], 1);
    assert!(page.preserve_big_integers);
}

#[tokio::test]
async fn flash_data_and_errors_are_delivered_to_the_next_render() {
    let session = ArraySession::new();
    let post = Inertia::with_session(
        config(),
        request(Method::POST, &[("x-inertia", "true")]),
        session.clone(),
    );

    post.flash("message", "Saved!")
        .with_errors(ValidationErrors::new().with("name", "Required."));
    post.clear_history();
    post.commit().await;

    let get = || {
        Inertia::with_session(
            config(),
            request(Method::GET, &[("x-inertia", "true")]),
            session.clone(),
        )
    };

    let page = resolve(get().render("Users", ())).await;
    assert_eq!(page.props["errors"], json!({ "name": "Required." }));
    assert_eq!(page.flash["message"], "Saved!");
    assert!(page.clear_history);

    let page = resolve(get().render("Users", ())).await;
    assert_eq!(page.props["errors"], json!({}));
    assert!(page.flash.is_empty());
    assert!(!page.clear_history);
}

#[tokio::test]
async fn a_failed_render_keeps_flash_data_and_errors_for_the_next() {
    let session = ArraySession::new();
    let inertia = || {
        Inertia::with_session(
            config(),
            request(Method::GET, &[("x-inertia", "true")]),
            session.clone(),
        )
    };
    let broken = || props! { "broken" => inertia::try_lazy(|| async { Err::<u8, _>("boom") }) };

    let post = inertia();
    post.flash("stored", 1)
        .with_errors(ValidationErrors::new().with("name", "Required."));
    post.commit().await;

    // Pending state queued in the failing request, as by a middleware.
    let get = inertia();
    get.flash("pending", 2)
        .with_errors_in("other", ValidationErrors::new().with("email", "Invalid."));
    get.clear_history();
    assert!(get.render("Users", broken()).into_page().await.is_err());
    get.commit().await;

    let page = resolve(inertia().render("Users", ())).await;
    assert_eq!(
        serde_json::to_value(&page.flash).unwrap(),
        json!({ "stored": 1, "pending": 2 })
    );
    assert_eq!(page.props["errors"], json!({ "name": "Required." }));
    assert!(page.clear_history);
}

#[tokio::test]
async fn flash_data_reaches_a_render_in_the_same_request() {
    let inertia = visit(&[]);
    inertia.flash("toast", "Hi").encrypt_history(true);

    let page = resolve(inertia.render("Users", ()).flash("other", 1)).await;

    assert_eq!(
        serde_json::to_value(&page.flash).unwrap(),
        json!({ "toast": "Hi", "other": 1 })
    );
    assert!(page.encrypt_history);
}

#[tokio::test]
async fn flash_data_from_prop_callbacks_reaches_the_current_page() {
    let session = ArraySession::new();
    let inertia = Inertia::with_session(config(), request(Method::GET, &[]), session.clone());
    inertia.flash("before", "Queued before rendering");
    let callback = inertia.clone();

    let page = resolve(
        inertia
            .render(
                "Users",
                props! {
                    "user" => inertia::lazy(move || async move {
                        callback.flash("toast", "Profile refreshed").flash("priority", "Callback");
                        "Taylor"
                    }),
                },
            )
            .flash("priority", "Response"),
    )
    .await;

    assert_eq!(page.flash["toast"], "Profile refreshed");
    assert_eq!(page.flash["before"], "Queued before rendering");
    assert_eq!(page.flash["priority"], "Response");

    inertia.commit().await;
    let next = Inertia::with_session(config(), request(Method::GET, &[]), session);
    assert!(resolve(next.render("Users", ())).await.flash.is_empty());
}

#[tokio::test]
async fn first_visits_render_the_root_view() {
    let config =
        config().root_view(|view: &inertia::View<'_>| format!("<title>{}</title>{}", view.page.component, view.body));
    let inertia = Inertia::new(config, request(Method::GET, &[]));

    let response = inertia
        .render("Users", props! { "html" => "</script><b>&" })
        .into_http()
        .await;
    let body = response.body();

    assert_eq!(response.headers()["content-type"], "text/html; charset=utf-8");
    assert!(body.starts_with("<title>Users</title>"));
    assert!(body.contains(r#"<div id="app"></div>"#));
    assert!(!body.contains("</script><b>"));
    AssertablePage::from_body(body).equals("html", "</script><b>&");
}

#[tokio::test]
async fn root_ids_are_escaped() {
    let inertia = Inertia::new(config().root_id(r#"app" onload="alert(1)"#), request(Method::GET, &[]));

    let response = inertia.render("Users", ()).into_http().await;
    let body = response.body();

    assert!(!body.contains(r#"" onload=""#), "{body}");
    assert!(
        body.contains(r#"<div id="app&quot; onload=&quot;alert(1)"></div>"#),
        "{body}"
    );
}

#[cfg(feature = "validator")]
#[test]
fn converts_validator_errors() {
    use validator::Validate;

    #[derive(Validate)]
    struct NewUser {
        #[validate(length(min = 3, message = "The name must be at least 3 characters."))]
        name: String,
        #[validate(email)]
        email_address: String,
        #[validate(nested)]
        address: Address,
        #[validate(nested)]
        phones: Vec<Phone>,
    }

    #[derive(Validate)]
    struct Address {
        #[validate(length(min = 1, message = "The city field is required."))]
        city: String,
    }

    #[derive(Validate)]
    struct Phone {
        #[validate(length(min = 7, message = "The number is too short."))]
        number: String,
    }

    let errors: ValidationErrors = NewUser {
        name: "Al".into(),
        email_address: "nope".into(),
        address: Address { city: String::new() },
        phones: vec![
            Phone {
                number: "5551234".into(),
            },
            Phone { number: "1".into() },
        ],
    }
    .validate()
    .unwrap_err()
    .into();

    assert_eq!(errors.first("name"), Some("The name must be at least 3 characters."));
    assert_eq!(
        errors.first("email_address"),
        Some("The email address field is invalid.")
    );
    assert_eq!(errors.first("address.city"), Some("The city field is required."));
    assert_eq!(errors.first("phones.1.number"), Some("The number is too short."));
    assert!(!errors.has("phones.0.number"));
}

#[cfg(feature = "garde")]
#[test]
fn converts_garde_reports() {
    use garde::Validate;

    #[derive(Validate)]
    struct NewUser {
        #[garde(length(min = 3))]
        name: String,
        #[garde(dive)]
        phones: Vec<Phone>,
    }

    #[derive(Validate)]
    struct Phone {
        #[garde(length(min = 7))]
        number: String,
    }

    let errors: ValidationErrors = NewUser {
        name: "Al".into(),
        phones: vec![
            Phone {
                number: "5551234".into(),
            },
            Phone { number: "1".into() },
        ],
    }
    .validate()
    .unwrap_err()
    .into();

    assert!(errors.has("name"));
    assert!(errors.has("phones.1.number"));
    assert!(!errors.has("phones[1].number"));
}

#[tokio::test]
async fn pages_carry_their_headers_as_json_and_as_documents() {
    let json = visit(&[])
        .render("Users", props! {})
        .with_header("x-page", "users")
        .with_header("x-page", "list")
        .try_into_http()
        .await
        .unwrap();
    assert_eq!(json.headers()["x-page"], "list");
    assert_eq!(json.headers().get_all("x-page").iter().count(), 1);

    let document = Inertia::new(config(), request(Method::GET, &[]))
        .render("Users", props! {})
        .with_headers([("x-page", "users"), ("cache-control", "no-store")])
        .encrypt_history(true)
        .try_into_http()
        .await
        .unwrap();
    assert_eq!(document.headers()["x-page"], "users");
    assert_eq!(document.headers()["cache-control"], "no-store");
}
