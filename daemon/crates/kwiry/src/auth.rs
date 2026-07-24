use std::sync::Arc;

use axum::Json;
use axum::extract::{Request, State};
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use kwiry_core::{ApiErrorEnvelope, Principal, token_matches};

use crate::capability::{CapabilityFailure, CapabilityVerifier};

#[derive(Clone)]
pub(crate) struct AuthState {
    method: AuthMethod,
}

#[derive(Clone)]
enum AuthMethod {
    Desktop(Arc<str>),
    OpenClast(Arc<CapabilityVerifier>),
}

impl AuthState {
    pub(crate) fn desktop(token: String) -> Self {
        Self {
            method: AuthMethod::Desktop(Arc::from(token)),
        }
    }

    pub(crate) fn openclast(verifier: CapabilityVerifier) -> Self {
        Self {
            method: AuthMethod::OpenClast(Arc::new(verifier)),
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
    let principal = match (&auth.method, supplied) {
        (AuthMethod::Desktop(expected), Some(supplied)) if token_matches(expected, supplied) => {
            Ok(Principal::desktop())
        }
        (AuthMethod::OpenClast(verifier), Some(supplied)) => verifier.verify(supplied),
        _ => Err(CapabilityFailure::Unauthorized),
    };

    match principal {
        Ok(principal) => {
            request.extensions_mut().insert(principal);
            next.run(request).await
        }
        Err(CapabilityFailure::Forbidden) => (
            StatusCode::FORBIDDEN,
            Json(ApiErrorEnvelope::new(
                "forbidden",
                "the capability does not authorize this request",
            )),
        )
            .into_response(),
        Err(CapabilityFailure::Unauthorized) => unauthorized(),
    }
}

fn unauthorized() -> Response {
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
    response
}
