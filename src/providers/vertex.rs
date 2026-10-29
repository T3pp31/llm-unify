use super::HttpProvider;
use crate::{Error, ErrorKind};
use google_cloud_auth::credentials::{CacheableResource, CredentialsProvider};

pub(super) async fn authenticate(
    provider: &HttpProvider,
    request: reqwest::RequestBuilder,
) -> Result<reqwest::RequestBuilder, Error> {
    let credentials = provider
        .credentials
        .get_or_try_init(|| async {
            google_cloud_auth::credentials::Builder::default()
                .with_scopes([provider
                    .config
                    .oauth_scope
                    .as_deref()
                    .ok_or_else(crate::config::configuration)?])
                .build_access_token_credentials()
                .map_err(|_| Error::new(ErrorKind::AuthError, "Google ADC initialization failed"))
        })
        .await?;
    let headers = credentials
        .headers(http::Extensions::new())
        .await
        .map_err(|_| Error::new(ErrorKind::AuthError, "Google ADC token refresh failed"))?;
    match headers {
        CacheableResource::New { mut data, .. } => {
            for value in data.values_mut() {
                value.set_sensitive(true);
            }
            Ok(request.headers(data))
        }
        CacheableResource::NotModified => {
            Err(Error::new(ErrorKind::AuthError, "ADC returned no headers"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChatRequest, Config, ContentBlock, Message, Provider, Role, ToolResult};
    use google_cloud_auth::credentials::{
        AccessToken, AccessTokenCredentials, AccessTokenCredentialsProvider, EntityTag,
    };
    use serde_json::json;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_partial_json, header, path},
    };

    #[derive(Debug)]
    struct TestCredentials(AtomicUsize);
    impl CredentialsProvider for TestCredentials {
        async fn headers(
            &self,
            _: http::Extensions,
        ) -> Result<CacheableResource<http::HeaderMap>, google_cloud_auth::errors::CredentialsError>
        {
            let count = self.0.fetch_add(1, Ordering::SeqCst);
            let mut data = http::HeaderMap::new();
            data.insert(
                "authorization",
                format!("Bearer test-token-{count}").parse().unwrap(),
            );
            data.insert("x-goog-user-project", "quota-project".parse().unwrap());
            Ok(CacheableResource::New {
                entity_tag: EntityTag::new(),
                data,
            })
        }
        async fn universe_domain(&self) -> Option<String> {
            None
        }
    }
    impl AccessTokenCredentialsProvider for TestCredentials {
        async fn access_token(
            &self,
        ) -> Result<AccessToken, google_cloud_auth::errors::CredentialsError> {
            Ok(AccessToken {
                token: "test-token".into(),
            })
        }
    }
    fn provider(server: &MockServer) -> HttpProvider {
        let settings = Arc::new(Config::default());
        let mut config = settings.providers["vertex"].clone();
        config.base_url = Some(server.uri());
        config.project = Some("test-project".into());
        config.location = Some("test-location".into());
        let provider = HttpProvider::new(
            "vertex".into(),
            config,
            settings,
            reqwest::Client::new(),
            None,
        );
        provider
            .credentials
            .set(AccessTokenCredentials::from(TestCredentials(
                AtomicUsize::new(0),
            )))
            .unwrap();
        provider
    }
    #[tokio::test]
    async fn vertex_auth_refresh_and_signed_tool_history() {
        let server = MockServer::start().await;
        let p = provider(&server);
        Mock::given(path("/projects/test-project/locations/test-location/publishers/google/models/model:generateContent")).and(header("authorization","Bearer test-token-0")).and(header("x-goog-user-project","quota-project")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"candidates":[{"content":{"parts":[{"functionCall":{"id":"c1","name":"weather","args":{"city":"Tokyo"}},"thoughtSignature":"sig"}]},"finishReason":"STOP"}]}))).expect(1).mount(&server).await;
        let response = p
            .chat(&ChatRequest::new("vertex/model").user("weather"))
            .await
            .unwrap();
        assert!(
            matches!(&response.content[0],ContentBlock::ToolCall(c) if c.metadata[0].data["thoughtSignature"]=="sig")
        );
        server.reset().await;
        Mock::given(path("/projects/test-project/locations/test-location/publishers/google/models/model:generateContent")).and(header("authorization","Bearer test-token-1")).and(body_partial_json(json!({"contents":[{"role":"user","parts":[{"text":"weather"}]},{"role":"model","parts":[{"functionCall":{"id":"c1","name":"weather","args":{"city":"Tokyo"}},"thoughtSignature":"sig"}]},{"role":"user","parts":[{"functionResponse":{"id":"c1","name":"weather","response":{"result":"sunny"}}}]}]}))).respond_with(ResponseTemplate::new(200).set_body_json(serde_json::from_str::<serde_json::Value>(include_str!("../../tests/fixtures/vertex.json")).unwrap())).expect(1).mount(&server).await;
        let request = ChatRequest::new("vertex/model")
            .user("weather")
            .message(response.into_message())
            .message(Message::tool(ToolResult::new("c1", json!("sunny"))));
        assert_eq!(p.chat(&request).await.unwrap().output_text(), "Hello 🌍");
    }
    #[tokio::test]
    async fn vertex_stream_handles_usage_and_completion() {
        let server = MockServer::start().await;
        let p = provider(&server);
        let body = format!(
            "data: {}\n\ndata: {}\n\n",
            json!({"candidates":[{"content":{"parts":[{"text":"hello"}]}}]}),
            json!({"candidates":[{"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":5,"candidatesTokenCount":2,"totalTokenCount":7}})
        );
        Mock::given(path("/projects/test-project/locations/test-location/publishers/google/models/model:streamGenerateContent")).respond_with(ResponseTemplate::new(200).set_body_raw(body,"text/event-stream")).expect(1).mount(&server).await;
        let response = p
            .chat_stream(
                &ChatRequest::new("vertex/model")
                    .message(Message::new(Role::User, vec![ContentBlock::text("hello")])),
            )
            .await
            .unwrap()
            .collect_response()
            .await
            .unwrap();
        assert_eq!(response.output_text(), "hello");
        assert_eq!(response.usage.unwrap().total_tokens, Some(7));
    }

    #[tokio::test]
    async fn vertex_synthetic_tool_ids_do_not_change_native_history() {
        let server = MockServer::start().await;
        let p = provider(&server);
        Mock::given(path("/projects/test-project/locations/test-location/publishers/google/models/model:generateContent")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"candidates":[{"content":{"parts":[{"functionCall":{"name":"weather","args":{"city":"Tokyo"}}},{"functionCall":{"name":"weather","args":{"city":"Osaka"}}}]},"finishReason":"STOP"}]}))).expect(2).mount(&server).await;
        let response = p
            .chat(&ChatRequest::new("vertex/model").user("weather"))
            .await
            .unwrap();
        let ids: Vec<String> = response
            .content
            .iter()
            .filter_map(|b| {
                if let ContentBlock::ToolCall(call) = b {
                    Some(call.id.clone())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1]);
        let request = ChatRequest::new("vertex/model")
            .user("weather")
            .message(response.into_message())
            .message(Message::tool(ToolResult::new(&ids[0], json!("sunny"))))
            .message(Message::tool(ToolResult::new(&ids[1], json!("cloudy"))));
        crate::capabilities::validate_request(&request).unwrap();
        let next = p.chat(&request).await.unwrap();
        assert!(matches!(&next.content[0],ContentBlock::ToolCall(c) if !ids.contains(&c.id)));
        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
        assert!(
            body["contents"][1]["parts"][0]["functionCall"]
                .get("id")
                .is_none()
        );
        assert_eq!(body["contents"][2]["parts"].as_array().unwrap().len(), 2);
        assert!(
            body["contents"][2]["parts"][0]["functionResponse"]
                .get("id")
                .is_none()
        );
    }
}
