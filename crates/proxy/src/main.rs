use clap::Parser;
use std::{net::SocketAddr, path::PathBuf};

#[derive(Parser)]
#[command(about = "Native LiteLLM compatibility proxy (parity work in progress)")]
struct Arguments {
    /// LiteLLM-compatible YAML configuration file.
    #[arg(long)]
    config: PathBuf,
    /// JSON execution limits and defaults; defaults to config/parity/native-defaults.json at build time.
    #[arg(long)]
    runtime_config: Option<PathBuf>,
    /// Explicit socket address; deployment addresses are not compiled into the binary.
    #[arg(long)]
    listen: SocketAddr,
    /// Validate settings and authentication without binding a port.
    #[arg(long)]
    check: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = Arguments::parse();
    let config = llm_unify_engine::load_config(arguments.config)?;
    let settings = match arguments.runtime_config {
        Some(path) => {
            llm_unify_engine::openai::Settings::from_json(&std::fs::read_to_string(path)?)?
        }
        None => llm_unify_engine::openai::Settings::bundled()?,
    };
    let app = llm_unify_proxy::router_with_settings(&config, settings).await?;
    if arguments.check {
        println!("configuration and native authentication validated");
        return Ok(());
    }
    let listener = tokio::net::TcpListener::bind(arguments.listen).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            #[cfg(unix)]
            {
                let mut terminate =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("install SIGTERM handler");
                tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
            }
            #[cfg(not(unix))]
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
