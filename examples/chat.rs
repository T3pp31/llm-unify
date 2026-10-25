use llm_unify::{ChatRequest, Client};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = std::env::var("LLM_UNIFY_MODEL")?;
    let client = Client::from_env()?;
    let response = client
        .chat(ChatRequest::new(model).user("Rust の特徴を教えてください。"))
        .await?;
    println!("{}", response.output_text());
    Ok(())
}
