use std::sync::Arc;

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{FromRequest, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use litellm_gateway_auth::{AccessRequest, AuthenticatedRequest};
use llm_unify_engine::{
    Gateway,
    openai::{Error, OpenAi},
};
use serde_json::Value;
use tower::ServiceExt;

#[derive(Clone)]
pub(crate) struct Chat {
    pub openai: OpenAi,
    pub gateway: Arc<Gateway>,
    pub fallback: Router,
}

pub(crate) async fn create(
    State(state): State<Chat>,
    identity: AuthenticatedRequest,
    request: Request,
) -> Response {
    let (parts, body) = request.into_parts();
    let body = match Bytes::from_request(Request::from_parts(parts.clone(), body), &state).await {
        Ok(body) => body,
        Err(rejection) => return rejection.into_response(),
    };
    let value: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return error(Error::Invalid("expected a JSON object")),
    };
    let Some(object) = value.as_object() else {
        return error(Error::Invalid("expected a JSON object"));
    };
    let name = object
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !state.openai.handles(name) {
        return state
            .fallback
            .oneshot(Request::from_parts(parts, Body::from(body)))
            .await
            .unwrap();
    }
    let deployment = state
        .gateway
        .models
        .get(name)
        .expect("deployment checked above");
    let access = AccessRequest::Model {
        name: name.into(),
        deployment: deployment.model.clone(),
    };
    if let Err(error) = identity
        .authorize(access.clone())
        .await
        .and_then(|authorized| authorized.consume(identity.caller(), &access))
    {
        return error.into_response();
    }
    if object.get("stream").and_then(Value::as_bool) == Some(true) {
        return match state.openai.chat_stream(name, object.clone()).await {
            Err(failure) => error(failure),
            Ok(mut stream) => {
                use futures_util::StreamExt;
                let bytes = async_stream::stream! {
                    while let Some(chunk) = stream.next().await {
                        match chunk {
                            Ok(chunk) => yield Ok::<_, std::io::Error>(chunk),
                            Err(failure) => {
                                let body = serde_json::json!({"error": {"message": failure.to_string(), "type": "api_error"}});
                                yield Ok(Bytes::from(format!("data: {body}\n\n")));
                                break;
                            }
                        }
                    }
                };
                (
                    [
                        (axum::http::header::CONTENT_TYPE, "text/event-stream"),
                        (axum::http::header::CACHE_CONTROL, "no-cache"),
                    ],
                    Body::from_stream(bytes),
                )
                    .into_response()
            }
        };
    }
    match state.openai.chat(name, object.clone()).await {
        Ok(response) => Json(response).into_response(),
        Err(failure) => error(failure),
    }
}

fn error(error: Error) -> Response {
    let status = match &error {
        Error::Invalid(_) => StatusCode::BAD_REQUEST,
        Error::Unsupported(_) => StatusCode::NOT_IMPLEMENTED,
        Error::MissingCredential => StatusCode::UNAUTHORIZED,
        Error::Configuration => StatusCode::INTERNAL_SERVER_ERROR,
        Error::Timeout => StatusCode::GATEWAY_TIMEOUT,
        Error::Upstream { status, .. } => {
            StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY)
        }
        _ => StatusCode::BAD_GATEWAY,
    };
    let message = error.to_string();
    let body = match error {
        Error::Upstream { body, .. } => body,
        _ => {
            serde_json::json!({"error": {"message": message, "type": "api_error", "param": null, "code": status.as_u16()}})
        }
    };
    (status, Json(body)).into_response()
}
