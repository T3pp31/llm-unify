use llm_unify::{ChatRequest, Client};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = std::env::var("LLM_UNIFY_MODEL")?;
    let client = Client::from_env()?;
    let response = client
        .chat(ChatRequest::new(model).user("What are the main features of Rust?"))
        .await?;
    println!("{}", response.output_text());
    Ok(())
}
