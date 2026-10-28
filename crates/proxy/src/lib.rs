//! Experimental compatibility HTTP frontend. Release eligibility is controlled by parity.
use axum::{
    Json, Router,
    extract::Request,
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use llm_unify_engine::{Auth, Config, Gateway};
use std::sync::Arc;

mod chat;
mod ui;

pub async fn router(config: &Config) -> Result<Router, llm_unify_engine::Error> {
    let settings = llm_unify_engine::openai::Settings::bundled()
        .map_err(|_| llm_unify_engine::Error::Invalid("invalid native HTTP settings"))?;
    router_with_settings(config, settings).await
}

pub async fn router_with_settings(
    config: &Config,
    settings: llm_unify_engine::openai::Settings,
) -> Result<Router, llm_unify_engine::Error> {
    let (gateway, auth) = llm_unify_engine::build(config).await?;
    let ui = ui::router(config, auth.clone())
        .map_err(|_| llm_unify_engine::Error::Invalid("invalid native UI manifest"))?;
    Ok(inference_router(gateway, auth, settings)
        .map_err(|_| llm_unify_engine::Error::Invalid("invalid native HTTP settings"))?
        .merge(ui))
}

fn inference_router(
    gateway: Arc<Gateway>,
    auth: Auth,
    settings: llm_unify_engine::openai::Settings,
) -> Result<Router, llm_unify_engine::openai::Error> {
    let fallback = litellm_gateway_inference::router(gateway.clone()).route_layer(
        axum::middleware::from_fn_with_state(auth.clone(), authenticate),
    );
    let chat = chat::Chat {
        openai: llm_unify_engine::openai::OpenAi::new(gateway.clone(), settings.clone())?,
        gateway,
        fallback: fallback.clone(),
    };
    Ok(Router::new()
        .route("/v1/chat/completions", post(chat::create))
        .route("/chat/completions", post(chat::create))
        .layer(axum::extract::DefaultBodyLimit::max(
            settings.max_request_bytes,
        ))
        .with_state(chat)
        .route_layer(axum::middleware::from_fn_with_state(auth, authenticate))
        .fallback_service(fallback)
        .route(
            "/health/readiness",
            get(|| async { Json(serde_json::json!({"status": "ready"})) }),
        )
        .route(
            "/health/liveliness",
            get(|| async { Json(serde_json::json!({"status": "alive"})) }),
        ))
}

async fn authenticate(
    axum::extract::State(auth): axum::extract::State<Auth>,
    request: Request,
    next: Next,
) -> Response {
    let (mut parts, body) = request.into_parts();
    match auth.authenticate_parts(&parts).await {
        Ok(identity) => {
            let route = litellm_gateway_auth::AccessRequest::Route {
                method: parts.method.to_string(),
                path: parts.extensions.get::<axum::extract::MatchedPath>().map_or(parts.uri.path(), axum::extract::MatchedPath::as_str).into(),
            };
            if let Err(error) = identity.authorize(route.clone()).await.and_then(|authorized| authorized.consume(identity.caller(), &route)) {
                return (error.status(), Json(serde_json::json!({"error":{"message": error.to_string(), "type":"permission_error", "param":null, "code":error.status().as_u16()}}))).into_response();
            }
            parts.extensions.insert(identity);
            next.run(Request::from_parts(parts, body)).await
        }
        Err(error) => (
            error.status(),
            Json(serde_json::json!({"error": {"message": error.to_string(), "type": "auth_error", "param": null, "code": error.status().as_u16()}})),
        ).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;

    async fn app() -> Router {
        let config = llm_unify_engine::parse_config(
            "model_list:\n  - model_name: demo\n    litellm_params:\n      model: openai/demo\ngeneral_settings:\n  master_key: test-only-key\n"
        ).unwrap();
        router(&config).await.unwrap()
    }

    #[tokio::test]
    async fn missing_and_invalid_keys_are_denied_before_inference() {
        for token in [None, Some("Bearer wrong")] {
            let mut request =
                Request::post("/v1/chat/completions").header("content-type", "application/json");
            if let Some(token) = token {
                request = request.header("authorization", token);
            }
            let response = app()
                .await
                .oneshot(request.body(Body::from("{}")).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), 401);
            let body: serde_json::Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap())
                    .unwrap();
            assert!(body.get("error").is_some());
        }
    }

    #[tokio::test]
    async fn valid_key_reaches_model_validation() {
        let response = app()
            .await
            .oneshot(
                Request::post("/v1/chat/completions")
                    .header("authorization", "Bearer test-only-key")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"unknown","messages":[]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
    }

    #[tokio::test]
    async fn responses_resolves_deployment_environment_before_sending_upstream() {
        use serde_json::json;
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::*};

        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(header("authorization", "Bearer test-upstream-secret"))
            .and(body_partial_json(
                json!({"model":"internal", "input":"hello"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id":"resp-fixture", "object":"response", "model":"internal", "status":"completed",
                "output":[{"type":"message", "content":[{"type":"output_text", "text":"OK"}]}],
                "usage":{"input_tokens":1, "output_tokens":1, "total_tokens":2}
            })))
            .expect(1)
            .mount(&upstream)
            .await;
        let config = llm_unify_engine::parse_config(&format!(
            "model_list:\n  - model_name: public\n    litellm_params:\n      model: openai/internal\n      api_base: os.environ/UPSTREAM_URL\n      api_key: os.environ/UPSTREAM_KEY\ngeneral_settings:\n  master_key: test-only-key\nenvironment_variables:\n  UPSTREAM_URL: {}/v1\n  UPSTREAM_KEY: test-upstream-secret\n", upstream.uri()
        )).unwrap();
        let response = router(&config)
            .await
            .unwrap()
            .oneshot(
                Request::post("/v1/responses")
                    .header("authorization", "Bearer test-only-key")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"public","input":"hello"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body = to_bytes(response.into_body(), 65536).await.unwrap();
        let output: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(output["output"][0]["content"][0]["text"], "OK");
        assert_eq!(output["usage"]["total_tokens"], 2);
        assert!(!String::from_utf8_lossy(&body).contains("test-upstream-secret"));
    }

    #[tokio::test]
    async fn liveness_does_not_require_credentials() {
        let response = app()
            .await
            .oneshot(
                Request::get("/health/liveliness")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
    }
}
