use std::sync::{Arc, Mutex, PoisonError};

use ::axum::body::Body;
use ::axum::response::{IntoResponse, Response};
use http::{StatusCode, header};

/// A page render waiting for the [`InertiaLayer`](super::InertiaLayer).
///
/// Response extensions must be `Clone + Sync`, which a render with pending
/// prop callbacks isn't, hence the shared, take-once slot.
#[derive(Clone)]
struct PendingRender(Arc<Mutex<Option<crate::Response>>>);

/// Returning a render from a handler hands it to the [`InertiaLayer`](super::InertiaLayer),
/// which resolves its props once the handler is done. Status codes, headers
/// and extensions set around the render are kept:
///
/// ```no_run
/// # use axum::{http::StatusCode, response::IntoResponse};
/// # use inertia::{Inertia, props};
/// async fn missing(inertia: Inertia) -> impl IntoResponse {
///     (StatusCode::NOT_FOUND, inertia.render("Error", props! { "status" => 404 }))
/// }
/// ```
impl IntoResponse for crate::Response {
    fn into_response(self) -> Response {
        let mut response = Response::default();
        response
            .extensions_mut()
            .insert(PendingRender(Arc::new(Mutex::new(Some(self)))));
        response
    }
}

/// Whether the response is a page render, waiting for the [`InertiaLayer`](super::InertiaLayer)
/// to resolve its props.
pub fn is_render(response: &Response) -> bool {
    response.extensions().get::<PendingRender>().is_some()
}

/// Render the page a handler returned, if any.
pub(super) async fn render(mut response: Response) -> Result<Response, crate::PropError> {
    let pending = response.extensions_mut().remove::<PendingRender>();
    let Some(page) = pending.and_then(|pending| pending.0.lock().unwrap_or_else(PoisonError::into_inner).take()) else {
        return Ok(response);
    };

    let (parts, _) = response.into_parts();
    let mut rendered = page.try_into_http().await?.map(Body::from);

    if rendered.status() == StatusCode::OK {
        *rendered.status_mut() = parts.status;
    }

    for (name, value) in &parts.headers {
        if name != header::CONTENT_TYPE && name != header::CONTENT_LENGTH {
            rendered.headers_mut().append(name, value.clone());
        }
    }
    rendered.extensions_mut().extend(parts.extensions);

    Ok(rendered)
}
