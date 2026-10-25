use futures_util::StreamExt;
use llm_unify::{ChatEvent, ChatRequest, Client};
use std::io::{self, Write};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = std::env::var("LLM_UNIFY_MODEL")?;
    let mut events = Client::from_env()?
        .chat_stream(ChatRequest::new(model).user("短い物語を書いてください。"))
        .await?;
    while let Some(event) = events.next().await {
        if let ChatEvent::TextDelta { delta, .. } = event? {
            print!("{delta}");
            io::stdout().flush()?;
        }
    }
    println!();
    Ok(())
}
