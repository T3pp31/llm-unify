use llm_unify::{ChatRequest, Client, ContentBlock, Message, ToolDefinition, ToolResult};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::from_env()?;
    let model = std::env::var("LLM_UNIFY_MODEL")?;
    let request = ChatRequest::new(model)
        .user("東京の天気を調べてください。")
        .tool(ToolDefinition::new(
            "weather",
            "Return weather for a city",
            json!({"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}),
        ));
    let response = client.chat(request.clone()).await?;
    if response.cache_hit {
        eprintln!("キャッシュ済みの Tool Call です。実行前に再利用の適否を判断してください。");
        return Ok(());
    }
    let mut next = request.message(response.clone().into_message());
    let mut called = false;
    for block in &response.content {
        if let ContentBlock::ToolCall(call) = block {
            // This example supplies a local mock result; real execution belongs to the application.
            let mut result = ToolResult::new(
                call.id.clone(),
                if call.name == "weather" {
                    json!({"weather":"sunny","source":"example mock"})
                } else {
                    json!({"error":"unknown tool"})
                },
            );
            result.is_error = call.name != "weather";
            next = next.message(Message::tool(result));
            called = true;
        }
    }
    if called {
        println!("{}", client.chat(next).await?.output_text());
    } else {
        println!("{}", response.output_text());
    }
    Ok(())
}
