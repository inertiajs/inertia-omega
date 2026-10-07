//! The Inertia view of an incoming request.

use http::{HeaderMap, HeaderName, Method, Uri, header};

use crate::header as inertia;

/// An incoming request, as far as the Inertia protocol is concerned.
///
/// Adapters build one per request from the method, URI and headers; the
/// protocol headers are parsed once, up front.
#[derive(Debug, Clone)]
pub struct Request {
    method: Method,
    url: String,
    full_url: String,
    headers: HeaderMap,
    is_inertia: bool,
    only: Option<Vec<String>>,
    except: Option<Vec<String>>,
    reset: Vec<String>,
    except_once_props: Vec<String>,
}

impl Request {
    /// Create a request from its method, URI and headers.
    pub fn new(method: Method, uri: &Uri, headers: HeaderMap) -> Self {
        let url = uri
            .path_and_query()
            .map_or_else(|| uri.path().to_owned(), |path| path.as_str().to_owned());

        let scheme = header_str(&headers, &HeaderName::from_static("x-forwarded-proto"))
            .and_then(forwarded_scheme)
            .or_else(|| uri.scheme_str())
            .unwrap_or("http");
        let host = header_str(&headers, &header::HOST)
            .map(str::to_owned)
            .or_else(|| uri.authority().map(ToString::to_string))
            .unwrap_or_else(|| "localhost".to_owned());

        Self {
            full_url: format!("{scheme}://{host}{url}"),
            url,
            is_inertia: headers.contains_key(inertia::INERTIA),
            only: header_list(&headers, &inertia::PARTIAL_ONLY),
            except: header_list(&headers, &inertia::PARTIAL_EXCEPT),
            reset: header_list(&headers, &inertia::RESET).unwrap_or_default(),
            except_once_props: header_list(&headers, &inertia::EXCEPT_ONCE_PROPS).unwrap_or_default(),
            method,
            headers,
        }
    }

    /// The request method.
    pub fn method(&self) -> &Method {
        &self.method
    }

    /// The path and query string.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The path, without the query string.
    pub fn path(&self) -> &str {
        self.url.split('?').next().unwrap_or("/")
    }

    /// The absolute URL, built from the `Host` and `X-Forwarded-Proto` headers.
    ///
    /// Both are taken as sent, so a proxy in front of the app should set
    /// them; only `http` and `https` are accepted as the scheme.
    pub fn full_url(&self) -> &str {
        &self.full_url
    }

    /// All request headers.
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// A request header, if present and valid UTF-8.
    pub fn header(&self, name: impl AsRef<str>) -> Option<&str> {
        self.headers.get(name.as_ref()).and_then(|value| value.to_str().ok())
    }

    /// Whether this is a visit made by the Inertia client.
    pub fn is_inertia(&self) -> bool {
        self.is_inertia
    }

    /// The asset version the client is running.
    pub fn version(&self) -> Option<&str> {
        header_str(&self.headers, &inertia::VERSION)
    }

    /// The component a partial reload is for.
    pub fn partial_component(&self) -> Option<&str> {
        header_str(&self.headers, &inertia::PARTIAL_COMPONENT).filter(|component| !component.is_empty())
    }

    /// Whether this is a partial reload of the given component.
    pub fn is_partial_reload_of(&self, component: &str) -> bool {
        self.partial_component() == Some(component)
    }

    /// The props a partial reload asked for.
    pub fn only(&self) -> Option<&[String]> {
        self.only.as_deref()
    }

    /// The props a partial reload excluded.
    pub fn except(&self) -> Option<&[String]> {
        self.except.as_deref()
    }

    /// The props whose merge state the client is resetting.
    pub fn reset(&self) -> &[String] {
        &self.reset
    }

    /// The once props the client already has.
    pub fn except_once_props(&self) -> &[String] {
        &self.except_once_props
    }

    /// Whether an infinite scroll request is loading a previous page.
    pub fn prepends_scroll(&self) -> bool {
        header_str(&self.headers, &inertia::INFINITE_SCROLL_MERGE_INTENT) == Some("prepend")
    }

    /// The error bag validation errors should be scoped to.
    pub fn error_bag(&self) -> Option<&str> {
        header_str(&self.headers, &inertia::ERROR_BAG).filter(|bag| !bag.is_empty())
    }

    /// Whether the client is prefetching the page: `Purpose`, `Sec-Purpose`
    /// or `X-Moz` is `prefetch`, in any case, as Laravel checks.
    pub fn is_prefetch(&self) -> bool {
        [inertia::PURPOSE, SEC_PURPOSE, X_MOZ]
            .iter()
            .any(|name| header_str(&self.headers, name).is_some_and(|value| value.eq_ignore_ascii_case("prefetch")))
    }

    /// The `Referer` header.
    pub fn referer(&self) -> Option<&str> {
        header_str(&self.headers, &header::REFERER)
    }
}

// Browsers' own prefetch headers, which aren't part of the Inertia protocol.
const SEC_PURPOSE: HeaderName = HeaderName::from_static("sec-purpose");
const X_MOZ: HeaderName = HeaderName::from_static("x-moz");

fn header_str<'a>(headers: &'a HeaderMap, name: &HeaderName) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// The scheme in an `X-Forwarded-Proto` header. Each proxy appends its own,
/// so the client's is the first; anything but HTTP's is ignored, since it
/// ends up in a URL the client is sent to.
fn forwarded_scheme(value: &str) -> Option<&'static str> {
    let first = value.split(',').next()?.trim();

    ["https", "http"]
        .into_iter()
        .find(|scheme| first.eq_ignore_ascii_case(scheme))
}

/// Parse a comma-separated header into a list, or `None` when absent or empty.
fn header_list(headers: &HeaderMap, name: &HeaderName) -> Option<Vec<String>> {
    let values: Vec<String> = header_str(headers, name)?
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect();

    (!values.is_empty()).then_some(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(headers: &[(&'static str, &'static str)]) -> Request {
        let headers = headers
            .iter()
            .map(|(name, value)| (HeaderName::from_static(name), value.parse().unwrap()))
            .collect();

        Request::new(Method::GET, &"/users?page=2".parse().unwrap(), headers)
    }

    #[test]
    fn parses_the_protocol_headers() {
        let request = request(&[
            ("host", "example.com"),
            ("x-inertia", "true"),
            ("x-inertia-partial-component", "Users/Index"),
            ("x-inertia-partial-data", "users, stats,"),
            ("x-inertia-reset", "users"),
        ]);

        assert!(request.is_inertia());
        assert_eq!(request.url(), "/users?page=2");
        assert_eq!(request.path(), "/users");
        assert_eq!(request.full_url(), "http://example.com/users?page=2");
        assert!(request.is_partial_reload_of("Users/Index"));
        assert_eq!(request.only(), Some(&["users".to_owned(), "stats".to_owned()][..]));
        assert_eq!(request.except(), None);
        assert_eq!(request.reset(), ["users"]);
    }

    #[test]
    fn the_scheme_is_the_first_forwarded_protocol() {
        let forwarded = |proto| {
            request(&[("host", "example.com"), ("x-forwarded-proto", proto)])
                .full_url()
                .to_owned()
        };

        assert_eq!(forwarded("https"), "https://example.com/users?page=2");
        assert_eq!(forwarded("HTTPS , http"), "https://example.com/users?page=2");
        assert_eq!(forwarded("javascript"), "http://example.com/users?page=2");
        assert_eq!(forwarded(""), "http://example.com/users?page=2");
    }

    #[test]
    fn a_plain_request_is_not_an_inertia_visit() {
        let request = request(&[]);

        assert!(!request.is_inertia());
        assert_eq!(request.partial_component(), None);
        assert_eq!(request.full_url(), "http://localhost/users?page=2");
    }

    #[test]
    fn prefetch_headers_are_read_in_any_case() {
        assert!(request(&[("purpose", "prefetch")]).is_prefetch());
        assert!(request(&[("purpose", "Prefetch")]).is_prefetch());
        assert!(request(&[("sec-purpose", "prefetch")]).is_prefetch());
        assert!(request(&[("x-moz", "PREFETCH")]).is_prefetch());
        assert!(!request(&[("purpose", "prerender")]).is_prefetch());
        assert!(!request(&[]).is_prefetch());
    }
}
