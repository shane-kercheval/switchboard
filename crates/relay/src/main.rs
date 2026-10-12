use std::net::{Ipv4Addr, SocketAddr};

use switchboard_relay::{Config, serve};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let config = Config::from_env()?;
    let addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, config.port));
    let listener = TcpListener::bind(addr).await?;
    tracing::info!(%addr, "relay listening");
    let shutdown = CancellationToken::new();
    tokio::spawn({
        let shutdown = shutdown.clone();
        async move {
            let _ = tokio::signal::ctrl_c().await;
            shutdown.cancel();
        }
    });
    serve(listener, config, shutdown).await?;
    Ok(())
}
