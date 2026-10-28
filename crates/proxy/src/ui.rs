//! Operator console; configuration responses allowlist public fields only.
use axum::{
    Json, Router,
    extract::{Request, State},
    http::{HeaderValue, header},
    middleware::Next,
    response::{Html, IntoResponse, Response},
    routing::get,
};
use llm_unify_engine::{Auth, Config};
use serde_json::{Value, json};
use std::sync::Arc;

pub fn router(config: &Config, auth: Auth) -> Result<Router, serde_json::Error> {
    let scope: Value = serde_json::from_str(include_str!("../../../config/proxy-ui.json"))?;
    let timeout: std::num::NonZeroU32 =
        serde_json::from_value(scope["request_timeout_ms"].clone())?;
    let script =
        include_str!("../ui/app.js").replace("__REQUEST_TIMEOUT_MS__", &timeout.to_string());
    // Deliberately allowlist public model names. Serializing Config can leak keys/URLs.
    let status = Arc::new(json!({
        "product": "LLM Unify",
        "version": env!("CARGO_PKG_VERSION"),
        "models": config.model_list.iter().map(|model| &model.model_name).collect::<Vec<_>>(),
        "endpoints": scope["endpoints"],
    }));
    let protected = Router::new()
        .route("/ui/api/status", get(status_response))
        .with_state(status)
        .route_layer(axum::middleware::from_fn_with_state(
            auth,
            crate::authenticate,
        ));
    Ok(Router::new()
        .route("/ui", get(index))
        .route("/ui/", get(index))
        .route(
            "/ui/app.js",
            get(move || {
                let script = script.clone();
                async move {
                    (
                        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                        script,
                    )
                }
            }),
        )
        .route("/ui/help", get(help))
        .route("/ui/style.css", get(styles))
        .merge(protected)
        .layer(axum::middleware::from_fn(response_headers)))
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../ui/index.html"))
}

async fn help() -> Html<&'static str> {
    Html(include_str!("../ui/help.html"))
}

async fn styles() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../ui/style.css"),
    )
}

async fn status_response(State(status): State<Arc<Value>>) -> Json<Value> {
    Json((*status).clone())
}

async fn response_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    for (name, value) in [
        ("cache-control", "no-store"),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
        ("x-frame-options", "DENY"),
        (
            "content-security-policy",
            "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
        ),
    ] {
        response
            .headers_mut()
            .insert(name, HeaderValue::from_static(value));
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    async fn app() -> Router {
        let config = llm_unify_engine::parse_config(
            "model_list:\n  - model_name: public-model\n    litellm_params:\n      model: openai/private-upstream\n      api_base: https://private.example/v1\n      api_key: provider-secret\ngeneral_settings:\n  master_key: console-test-key\n",
        ).unwrap();
        crate::router(&config).await.unwrap()
    }

    #[tokio::test]
    async fn status_denies_missing_and_invalid_credentials() {
        for key in [None, Some("Bearer wrong")] {
            let mut request = Request::get("/ui/api/status");
            if let Some(key) = key {
                request = request.header("authorization", key);
            }
            let response = app()
                .await
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(response.headers()["cache-control"], "no-store");
        }
    }

    #[tokio::test]
    async fn authenticated_status_exposes_only_public_model_names() {
        let response = app()
            .await
            .oneshot(
                Request::get("/ui/api/status")
                    .header("authorization", "Bearer console-test-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 16384).await.unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        let status: Value = serde_json::from_str(text).unwrap();
        assert_eq!(status["product"], "LLM Unify");
        assert!(status.get("runtime").is_none());
        assert!(status.get("limitations").is_none());
        assert_eq!(status["models"], json!(["public-model"]));
        for secret in [
            "console-test-key",
            "provider-secret",
            "private-upstream",
            "private.example",
        ] {
            assert!(!text.contains(secret));
        }
    }

    #[tokio::test]
    async fn static_console_has_security_headers_and_no_configuration() {
        for (path, content_type) in [
            ("/ui", "text/html"),
            ("/ui/", "text/html"),
            ("/ui/help", "text/html"),
            ("/ui/app.js", "text/javascript"),
            ("/ui/style.css", "text/css"),
        ] {
            let response = app()
                .await
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert!(
                response.headers()["content-type"]
                    .to_str()
                    .unwrap()
                    .starts_with(content_type)
            );
            assert_eq!(response.headers()["cache-control"], "no-store");
            assert_eq!(response.headers()["x-content-type-options"], "nosniff");
            assert!(
                response.headers()["content-security-policy"]
                    .to_str()
                    .unwrap()
                    .contains("frame-ancestors 'none'")
            );
            let body = to_bytes(response.into_body(), 32768).await.unwrap();
            let body = std::str::from_utf8(&body).unwrap();
            assert!(!body.contains("provider-secret"));
            assert!(!body.contains("console-test-key"));
            assert!(!body.contains("private.example"));
        }
    }
}
