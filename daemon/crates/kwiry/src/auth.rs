use std::sync::Arc;

use axum::Json;
use axum::extract::{Request, State};
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use kwiry_core::{ApiErrorEnvelope, Principal, token_matches};

#[derive(Clone)]
pub(crate) struct AuthState {
    token: Arc<str>,
}

impl AuthState {
    pub(crate) fn new(token: String) -> Self {
        Self {
            token: Arc::from(token),
        }
    }
}

pub(crate) async fn require_auth(
    State(auth): State<AuthState>,
    mut request: Request,
    next: Next,
) -> Response {
    let supplied = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if !supplied.is_some_and(|supplied| token_matches(&auth.token, supplied)) {
        let mut response = (
            StatusCode::UNAUTHORIZED,
            Json(ApiErrorEnvelope::new(
                "unauthorized",
                "a valid bearer token is required",
            )),
        )
            .into_response();
        response
            .headers_mut()
            .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        return response;
    }

    request.extensions_mut().insert(Principal::desktop());
    next.run(request).await
}
