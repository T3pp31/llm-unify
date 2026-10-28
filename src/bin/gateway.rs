use llm_unify::gateway::Gateway;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let gateway = Gateway::from_env().await?;
    let listener = tokio::net::TcpListener::bind(gateway.listen_address()).await?;
    eprintln!("llm-unify gateway listening on {}", listener.local_addr()?);
    axum::serve(listener, gateway.router())
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
