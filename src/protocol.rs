//! The protocol rules an adapter applies around each handler, like the
//! Laravel adapter's middleware.
//!
//! An adapter calls [`before`] ahead of the handler and [`after`] on its
//! response; everything else is framework-agnostic.

use http::{HeaderMap, HeaderValue, Method, StatusCode, header, response};

use crate::HttpResponse;
use crate::config::Config;
use crate::header as inertia;
use crate::request::Request;

/// Run ahead of the handler. Returns the response to send instead of
/// running it: when an Inertia `GET` visit comes from a client with
/// outdated assets, a `409 Conflict` that has it reload the page.
pub fn before(request: &Request, config: &Config) -> Option<HttpResponse> {
    version_conflict(request, || config.current_version())
}

/// [`before`], computing the version only for the requests it checks.
pub(crate) fn version_conflict<V: AsRef<str>>(request: &Request, version: impl FnOnce() -> V) -> Option<HttpResponse> {
    if !request.is_inertia() || request.method() != Method::GET {
        return None;
    }

    let version = version();
    let version = version.as_ref();

    if request.version().unwrap_or_default() == version {
        return None;
    }

    let mut response = location(request, request.full_url());

    if let Ok(version) = HeaderValue::try_from(version) {
        response.headers_mut().insert(inertia::VERSION, version);
    }

    Some(response)
}

/// Run on the handler's response. Adds `Vary: X-Inertia` and, for Inertia
/// visits:
///
/// - turns an empty `200` into a redirect back,
/// - turns a `302` after `PUT`, `PATCH` or `DELETE` into a `303`, so the
///   browser follows it with a `GET`,
/// - turns a redirect to a URL with a fragment into a `409 Conflict` with
///   `X-Inertia-Redirect`, since `fetch` drops fragments when following.
///
/// Returns the response that replaces the handler's, if any.
pub fn after(request: &Request, response: &mut response::Parts, body_is_empty: bool) -> Option<HttpResponse> {
    vary(&mut response.headers);

    if !request.is_inertia() {
        return None;
    }

    if response.status == StatusCode::OK && body_is_empty {
        let mut back = redirect(request.referer().unwrap_or("/"));
        see_other_after_mutation(request, back.status_mut());
        return Some(back);
    }

    see_other_after_mutation(request, &mut response.status);

    if is_redirect(response.status) && !request.is_prefetch() {
        let location = response
            .headers
            .get(header::LOCATION)
            .filter(|location| location.as_bytes().contains(&b'#'));

        if let Some(location) = location {
            return Some(with_header(StatusCode::CONFLICT, inertia::REDIRECT, location.clone()));
        }
    }

    None
}

/// A `302 Found` redirect. After `PUT`, `PATCH` and `DELETE` Inertia visits,
/// [`after`] turns it into a `303 See Other`.
#[must_use = "a redirect does nothing unless it's returned"]
pub fn redirect(to: &str) -> HttpResponse {
    match HeaderValue::try_from(to) {
        Ok(to) => with_header(StatusCode::FOUND, header::LOCATION, to),
        Err(_) => invalid_url(to),
    }
}

/// A redirect the client follows with a full page load: a `409 Conflict`
/// with `X-Inertia-Location` for Inertia visits, a plain redirect otherwise.
#[must_use = "a redirect does nothing unless it's returned"]
pub fn location(request: &Request, url: &str) -> HttpResponse {
    if !request.is_inertia() {
        return redirect(url);
    }

    match HeaderValue::try_from(url) {
        Ok(url) => with_header(StatusCode::CONFLICT, inertia::LOCATION, url),
        Err(_) => invalid_url(url),
    }
}

fn see_other_after_mutation(request: &Request, status: &mut StatusCode) {
    if *status == StatusCode::FOUND && matches!(*request.method(), Method::PUT | Method::PATCH | Method::DELETE) {
        *status = StatusCode::SEE_OTHER;
    }
}

fn is_redirect(status: StatusCode) -> bool {
    matches!(status.as_u16(), 201 | 301 | 302 | 303 | 307 | 308)
}

/// Add `Vary: X-Inertia`, unless the response already varies on it.
fn vary(headers: &mut HeaderMap) {
    let varies = headers
        .get_all(header::VARY)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| {
            value
                .split(',')
                .any(|name| name.trim().eq_ignore_ascii_case("x-inertia"))
        });

    if !varies {
        headers.append(header::VARY, HeaderValue::from_static("X-Inertia"));
    }
}

fn with_header(status: StatusCode, name: http::HeaderName, value: HeaderValue) -> HttpResponse {
    let mut response = HttpResponse::default();
    *response.status_mut() = status;
    response.headers_mut().insert(name, value);
    vary(response.headers_mut());
    response
}

fn invalid_url(url: &str) -> HttpResponse {
    tracing::error!(url, "cannot redirect to a URL that isn't a valid header value");

    let mut response = HttpResponse::new("Internal Server Error".to_owned());
    *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
    response
}

#[cfg(test)]
mod tests {
    use http::HeaderName;

    use super::*;

    fn request(method: Method, headers: &[(&'static str, &'static str)]) -> Request {
        let headers = headers
            .iter()
            .map(|(name, value)| (HeaderName::from_static(name), value.parse().unwrap()))
            .collect();

        Request::new(method, &"/users".parse().unwrap(), headers)
    }

    fn parts(status: StatusCode, location: Option<&'static str>) -> response::Parts {
        let mut response = HttpResponse::default();
        *response.status_mut() = status;
        if let Some(location) = location {
            response
                .headers_mut()
                .insert(header::LOCATION, HeaderValue::from_static(location));
        }
        response.into_parts().0
    }

    #[test]
    fn outdated_clients_reload_the_page() {
        let config = Config::new().version("2");
        let request = request(
            Method::GET,
            &[("x-inertia", "true"), ("x-inertia-version", "1"), ("host", "app.test")],
        );

        let response = before(&request, &config).unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(response.headers()["x-inertia-location"], "http://app.test/users");
        assert_eq!(response.headers()["x-inertia-version"], "2");
    }

    #[test]
    fn only_get_visits_are_checked_for_outdated_assets() {
        let config = Config::new().version("2");

        assert!(before(&request(Method::POST, &[("x-inertia", "true")]), &config).is_none());
        assert!(before(&request(Method::GET, &[]), &config).is_none());
    }

    #[test]
    fn redirects_after_mutations_become_see_other() {
        let mut response = parts(StatusCode::FOUND, Some("/users"));

        assert!(after(&request(Method::PUT, &[("x-inertia", "true")]), &mut response, true).is_none());
        assert_eq!(response.status, StatusCode::SEE_OTHER);
        assert_eq!(response.headers["vary"], "X-Inertia");
    }

    #[test]
    fn redirects_with_fragments_become_conflicts() {
        let request = request(Method::POST, &[("x-inertia", "true")]);
        let replacement = after(&request, &mut parts(StatusCode::FOUND, Some("/users#top")), true).unwrap();

        assert_eq!(replacement.status(), StatusCode::CONFLICT);
        assert_eq!(replacement.headers()["x-inertia-redirect"], "/users#top");
    }

    #[test]
    fn prefetches_follow_redirects_with_fragments() {
        // A prefetched page is only cached, so there's no visit whose
        // fragment `fetch` could drop.
        let request = request(Method::GET, &[("x-inertia", "true"), ("purpose", "prefetch")]);
        let mut response = parts(StatusCode::FOUND, Some("/users#top"));

        assert!(after(&request, &mut response, true).is_none());
        assert_eq!(response.status, StatusCode::FOUND);
        assert_eq!(response.headers["location"], "/users#top");
    }

    #[test]
    fn empty_responses_redirect_back() {
        let request = request(Method::PATCH, &[("x-inertia", "true"), ("referer", "/users/1/edit")]);
        let replacement = after(&request, &mut parts(StatusCode::OK, None), true).unwrap();

        assert_eq!(replacement.status(), StatusCode::SEE_OTHER);
        assert_eq!(replacement.headers()["location"], "/users/1/edit");
    }

    #[test]
    fn plain_requests_only_gain_the_vary_header() {
        let mut response = parts(StatusCode::FOUND, Some("/users#top"));

        assert!(after(&request(Method::PUT, &[]), &mut response, true).is_none());
        assert_eq!(response.status, StatusCode::FOUND);
        assert_eq!(response.headers["vary"], "X-Inertia");
    }
}
