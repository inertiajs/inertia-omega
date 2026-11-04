//! The framework-agnostic core, exercised without a web framework.

use std::sync::Arc;
use std::time::Duration;

use http::{HeaderMap, HeaderName, Method};
use inertia::session::ArraySession;
use inertia::testing::AssertablePage;
use inertia::{
    Config, Inertia, LiveChannel, LiveListener, Page, Props, ProvidesScrollMetadata, Request, ScrollMetadata,
    ValidationErrors, props,
};
use serde::Serialize;
use serde_json::json;

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

/// Flashes when dropped, as a value an application's prop callback captures might.
struct FlashWhenDropped(Inertia);

impl Drop for FlashWhenDropped {
    fn drop(&mut self) {
        self.0.flash("dropped", true);
    }
}

fn flashing_prop(inertia: &Inertia) -> inertia::Prop {
    let flash = FlashWhenDropped(inertia.clone());

    inertia::lazy(move || async move {
        drop(flash);
        "Taylor"
    })
}

#[test]
fn replacing_a_shared_prop_drops_the_old_one_outside_the_lock() {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let inertia = visit(&[]);
        inertia.share("user", flashing_prop(&inertia));
        inertia.share("user", "Jess");
        sender.send(inertia).unwrap();
    });
    let inertia = receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("sharing must drop the replaced prop after releasing the pending-data lock");

    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let page = runtime.block_on(resolve(inertia.render("Users", props! {})));
    assert_eq!(page.props["user"], json!("Jess"));
    assert_eq!(page.flash["dropped"], json!(true));
}

#[test]
fn committing_drops_unrendered_shared_props_outside_the_lock() {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let inertia = Inertia::with_session(config(), request(Method::POST, &[]), ArraySession::new());
        inertia.share("user", flashing_prop(&inertia));
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        runtime.block_on(inertia.commit());
        sender.send(()).unwrap();
    });

    receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("committing must drop shared props no page rendered after releasing the pending-data lock");
}

#[tokio::test]
async fn partial_reloads_exclude_the_excepted_props() {
    let page =
        resolve(reload(&[("x-inertia-partial-except", "auth,literal.x,errors")]).render("Users", users_props())).await;
    let page = AssertablePage::from_body(&serde_json::to_string(&page).unwrap());

    page.missing("auth")
        .has("errors")
        .equals("literal", json!({ "y": { "z": 2 } }))
        .equals("optional", "optional");
}

#[tokio::test]
async fn partial_reloads_of_another_component_are_full_visits() {
    let page = resolve(reload(&[("x-inertia-partial-data", "users")]).render("Dashboard", users_props())).await;

    assert_eq!(page.props["appName"], "Demo");
    assert!(page.props.get("optional").is_none());
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
async fn lazy_props_take_callbacks_that_return_results() {
    let stats = || async { Ok::<_, std::io::Error>(42) };
    let broken = || async { Err::<u8, _>(std::io::Error::other("The stats are down")) };
    let props = || {
        props! {
            "stats" => inertia::defer(stats),
            "broken" => inertia::defer(broken).rescue(),
        }
    };

    let page = resolve(reload(&[("x-inertia-partial-data", "stats,broken")]).render("Users", props())).await;
    assert_eq!(
        serde_json::to_value(&page.props).unwrap(),
        json!({ "errors": {}, "stats": 42 })
    );
    assert_eq!(page.metadata.rescued_props, ["broken"]);

    let response = visit(&[]).render("Users", props! { "broken" => inertia::lazy(broken) });
    assert_eq!(
        response.into_page().await.unwrap_err().to_string(),
        "The stats are down"
    );
}

#[tokio::test]
async fn unrescued_failures_fail_the_render() {
    let response = visit(&[]).render(
        "Users",
        props! { "broken" => inertia::try_lazy(|| async { Err::<u8, _>("boom") }) },
    );

    assert_eq!(response.into_page().await.unwrap_err().to_string(), "boom");

    for broken in [
        inertia::lazy(async || Err::<u8, _>(std::io::Error::other("disk"))),
        inertia::try_lazy(async || Err::<u8, _>(std::io::Error::other("disk"))),
    ] {
        let error = visit(&[])
            .render("Users", props! { broken => broken })
            .into_page()
            .await
            .unwrap_err();

        assert_eq!(error.type_name(), std::any::type_name::<std::io::Error>());
        assert!(error.into_inner().is::<std::io::Error>());
    }
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
async fn scroll_props_carry_pagination_metadata() {
    let props = || {
        props! {
            "users" => inertia::scroll(Results::page((11..=20).collect(), 2, 3)),
            "later" => inertia::scroll_with(|| async { Results::page((1..=5).collect(), 1, 1) }).deferred(),
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

/// A page of results, as an application's paginator serializes one.
#[derive(Serialize)]
struct Results {
    data: Vec<u64>,
    #[serde(skip)]
    scroll: ScrollMetadata,
}

impl Results {
    fn page(data: Vec<u64>, current_page: u64, last_page: u64) -> Self {
        Self {
            data,
            scroll: ScrollMetadata::pages("page", current_page, last_page),
        }
    }
}

impl ProvidesScrollMetadata for Results {
    fn scroll_metadata(&self) -> ScrollMetadata {
        self.scroll.clone()
    }
}

#[tokio::test]
async fn sibling_callbacks_resolve_concurrently() {
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
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
async fn a_failed_render_leaves_flash_data_and_errors_for_the_next_one() {
    let session = ArraySession::new();
    let post = Inertia::with_session(config(), request(Method::POST, &[]), session.clone());
    post.flash("message", "Saved!")
        .with_errors(ValidationErrors::new().with("name", "Required."));
    post.commit().await;

    let get = || Inertia::with_session(config(), request(Method::GET, &[]), session.clone());
    let failing = props! { "stats" => inertia::try_lazy(async || Err::<u32, _>("the database is down")) };

    let inertia = get();
    inertia.encrypt_history(true).flash("toast", "Hi");
    assert!(inertia.render("Users", failing).into_page().await.is_err());

    let error_page = resolve(inertia.render("Error", ())).await;
    assert_eq!(error_page.flash["message"], "Saved!");
    assert_eq!(error_page.flash["toast"], "Hi");
    assert_eq!(error_page.props["errors"], json!({ "name": "Required." }));
    assert!(error_page.encrypt_history);

    let inertia = get();
    inertia.flash("toast", "Hi");
    let failing = props! { "stats" => inertia::try_lazy(async || Err::<u32, _>("the database is down")) };
    assert!(inertia.render("Users", failing).into_page().await.is_err());
    inertia.commit().await;

    let next = resolve(get().render("Users", ())).await;
    assert_eq!(next.flash["toast"], "Hi");
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

#[tokio::test]
async fn props_take_variables_by_name_beside_named_and_quoted_pairs() {
    let users = vec!["Taylor"];
    let r#type = "admin";
    let props = props! { users, can_edit => true, r#type, "auth.user.id" => 1 };

    let page = resolve(visit(&[]).render("Users", props)).await;

    assert_eq!(page.props["users"], json!(["Taylor"]));
    assert_eq!(page.props["can_edit"], json!(true));
    assert_eq!(page.props["type"], json!("admin"));
    assert_eq!(page.props["auth"]["user"]["id"], json!(1));
}

#[tokio::test]
async fn conditional_props_are_only_sent_when_their_condition_holds() {
    let (role, team, props) = ("editor", Some("Laravel"), vec![1, 2]);
    let page = props! {
        if role == "admin" {
            stats => 1,
        } else if matches!(role, "editor" | "author") {
            drafts => props.len(),
            if let Some(team) = team { team }
        } else {
            upgrade => true,
        }
        "auth.role" => role,
    };

    let page = resolve(visit(&[]).render("Users", page)).await;

    assert_eq!(page.props["drafts"], json!(2));
    assert_eq!(page.props["team"], json!("Laravel"));
    assert_eq!(page.props["auth"]["role"], json!("editor"));
    for missing in ["stats", "upgrade"] {
        assert!(page.props.get(missing).is_none(), "{missing}: {:?}", page.props);
    }
}

fn order_listeners() -> Vec<LiveListener> {
    vec![LiveListener::new(LiveChannel::private("orders.1"), ["OrderUpdated"])]
}

/// The `liveProps` of a page, as the client receives them.
fn live_props(page: &Page) -> serde_json::Value {
    serde_json::to_value(page).unwrap()["liveProps"].clone()
}

#[tokio::test]
async fn live_props_announce_their_listeners() {
    let page = resolve(visit(&[]).render(
        "Users",
        props! {
            order => inertia::lazy(async || "bar").live(order_listeners()),
            metrics => inertia::live("baz", order_listeners()).throttle(Duration::from_millis(500)),
            plain => "qux",
        },
    ))
    .await;

    assert_eq!(page.props["order"], "bar");
    assert_eq!(page.props["metrics"], "baz");
    assert_eq!(
        live_props(&page),
        json!({
            "order": {
                "listeners": [{ "channel": { "name": "orders.1", "type": "private" }, "events": ["OrderUpdated"] }],
            },
            "metrics": {
                "listeners": [{ "channel": { "name": "orders.1", "type": "private" }, "events": ["OrderUpdated"] }],
                "throttle": 500,
            },
        })
    );

    let page = resolve(visit(&[]).render("Users", props! { plain => "qux" })).await;
    assert!(serde_json::to_value(&page).unwrap().get("liveProps").is_none());
}

#[tokio::test]
async fn live_listeners_on_the_same_channel_collapse() {
    let order = inertia::live(
        "bar",
        [
            LiveListener::new(LiveChannel::private("orders.1"), ["OrderUpdated", "OrderUpdated"]),
            LiveListener::new(LiveChannel::public("orders.1"), ["OrderUpdated"]),
            LiveListener::new(LiveChannel::presence("orders.1"), Vec::<String>::new()),
        ],
    )
    .throttle(Duration::from_secs(5))
    .live([
        LiveListener::new(LiveChannel::private("orders.1"), ["OrderShipped", "OrderUpdated"]),
        LiveListener::new(LiveChannel::encrypted_private("orders.1"), ["OrderPaid"]),
    ]);

    let page = resolve(visit(&[]).render("Users", props! { order => order })).await;

    assert_eq!(
        live_props(&page),
        json!({
            "order": {
                "listeners": [
                    { "channel": { "name": "orders.1", "type": "private" }, "events": ["OrderUpdated", "OrderShipped"] },
                    { "channel": { "name": "orders.1", "type": "public" }, "events": ["OrderUpdated"] },
                    { "channel": { "name": "orders.1", "type": "encrypted-private" }, "events": ["OrderPaid"] },
                ],
                "throttle": 5000,
            },
        })
    );
}

#[tokio::test]
async fn props_left_out_of_the_first_visit_are_still_live() {
    let props = || {
        props! {
            deferred => inertia::defer(async || 1).live(order_listeners()),
            optional => inertia::optional(async || 2).live(order_listeners()),
            always => inertia::always(3).live(order_listeners()),
            once => inertia::once(async || 4).live(order_listeners()),
            rescued => inertia::try_lazy(async || Err::<u8, _>("boom")).deferred().rescue().live(order_listeners()),
        }
    };

    let page = resolve(visit(&[("x-inertia-except-once-props", "once")]).render("Users", props())).await;

    assert_eq!(page.props.get("always"), Some(&json!(3)));
    assert!(page.props.get("deferred").is_none());
    assert!(page.props.get("optional").is_none());
    assert!(page.props.get("once").is_none());
    assert_eq!(
        page.metadata.live_props.keys().collect::<Vec<_>>(),
        ["deferred", "optional", "always", "once", "rescued"]
    );
}

#[tokio::test]
async fn partial_reloads_only_announce_the_listeners_they_send() {
    let props = || {
        props! {
            order => inertia::always("bar").live(order_listeners()),
            metrics => inertia::lazy(async || "baz").live(order_listeners()),
            status => inertia::lazy(async || "ready").live(order_listeners()),
            summary => inertia::always(props! {
                total => inertia::live(100, order_listeners()),
            }),
        }
    };

    let page = resolve(reload(&[("x-inertia-partial-data", "metrics")]).render("Users", props())).await;
    assert_eq!(
        serde_json::to_value(&page.props).unwrap(),
        json!({ "errors": {}, "order": "bar", "metrics": "baz", "summary": { "total": 100 } })
    );
    assert_eq!(
        page.metadata.live_props.keys().collect::<Vec<_>>(),
        ["order", "metrics", "summary.total"]
    );

    let page = resolve(reload(&[("x-inertia-partial-except", "status,summary")]).render("Users", props())).await;
    assert_eq!(
        page.metadata.live_props.keys().collect::<Vec<_>>(),
        ["order", "metrics", "summary.total"],
        "an always prop outranks except, so its children's listeners are sent"
    );

    let page = resolve(reload(&[("x-inertia-partial-except", "metrics")]).render(
        "Users",
        props! {
            metrics => inertia::lazy(async || "baz").live(order_listeners()),
            status => inertia::lazy(async || "ready").live(order_listeners()),
        },
    ))
    .await;
    assert_eq!(page.metadata.live_props.keys().collect::<Vec<_>>(), ["status"]);
}

#[tokio::test]
async fn a_rescued_live_prop_that_failed_announces_nothing() {
    let page = resolve(reload(&[("x-inertia-partial-data", "orders")]).render(
        "Users",
        props! {
            orders => inertia::try_lazy(async || Err::<u8, _>("boom")).deferred().rescue().live(order_listeners()),
        },
    ))
    .await;

    assert_eq!(page.metadata.rescued_props, ["orders"]);
    assert!(page.metadata.live_props.is_empty());
}

#[tokio::test]
async fn nested_live_props_are_keyed_by_their_path() {
    let page = resolve(visit(&[]).render(
        "Users",
        props! {
            order => props! { total => inertia::live(100, order_listeners()) },
            "invoice.total" => inertia::live(200, order_listeners()),
        },
    ))
    .await;

    assert_eq!(page.props["order"], json!({ "total": 100 }));
    assert_eq!(page.props["invoice"], json!({ "total": 200 }));
    assert_eq!(
        page.metadata.live_props.keys().collect::<Vec<_>>(),
        ["order.total", "invoice.total"]
    );
}

#[tokio::test]
async fn live_props_that_merge_or_listen_for_nothing_fail_the_render() {
    let failures = [
        (
            "merged",
            inertia::merge(vec![1]).live(order_listeners()),
            "live props can't merge",
        ),
        (
            "deep",
            inertia::live(1, order_listeners()).deep_merge(),
            "live props can't merge",
        ),
        (
            "paths",
            inertia::live(1, order_listeners()).append_at("data"),
            "live props can't merge",
        ),
        (
            "matched",
            inertia::live(1, order_listeners()).match_on("id"),
            "live props can't merge",
        ),
        (
            "scroll",
            inertia::scroll(Results::page(vec![1], 1, 1)).live(order_listeners()),
            "live props can't merge",
        ),
        (
            "throttled",
            inertia::lazy(async || 1).throttle(Duration::from_secs(1)),
            "at least one event",
        ),
        ("empty", inertia::live(1, []), "at least one event"),
    ];

    for (key, prop, message) in failures {
        let error = visit(&[])
            .render("Users", Props::new().with(key, prop))
            .into_page()
            .await
            .unwrap_err();

        assert!(error.to_string().contains(key), "{error}");
        assert!(error.to_string().contains(message), "{error}");
    }
}

#[test]
fn broadcast_props_wrap_the_props_in_the_payload() {
    let payload = inertia::broadcast_props(
        json!({ "order_id": 1 }),
        json!({ "order": { "status": "shipped" }, "order.total": 100 }),
    )
    .unwrap();

    assert_eq!(
        payload,
        json!({
            "order_id": 1,
            "__inertia": { "props": { "order": { "status": "shipped" }, "order.total": 100 } },
        })
    );
}

#[test]
fn broadcast_props_need_json_objects() {
    let error = inertia::broadcast_props(json!([1]), json!({ "order": 1 })).unwrap_err();
    assert!(error.to_string().contains("payload must be a JSON object"), "{error}");

    let error = inertia::broadcast_props(json!({}), "order").unwrap_err();
    assert!(
        error.to_string().contains("props must serialize to a JSON object"),
        "{error}"
    );
}

#[tokio::test]
async fn once_and_live_props_take_spans() {
    use jiff::ToSpan;

    let secs = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    };
    let before = secs();
    let page = resolve(visit(&[]).render(
        "Users",
        props! {
            plans => inertia::once(async || 1).until(1.day()),
            countries => inertia::once(async || 2).until(1.month()),
            metrics => inertia::live(3, order_listeners()).throttle(5.seconds()),
        },
    ))
    .await;
    let after = secs();

    let expires_at = |prop: &str| page.metadata.once_props[prop].expires_at.unwrap() / 1000;
    assert!((before + 86_400..=after + 86_400).contains(&expires_at("plans")));
    assert!((before + 28 * 86_400..=after + 31 * 86_400).contains(&expires_at("countries")));
    assert_eq!(page.metadata.live_props["metrics"].throttle, Some(5000));
}
