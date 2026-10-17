use std::io::IsTerminal;
use std::net::{Ipv4Addr, SocketAddr};

use switchboard_relay::{Config, serve};
use tokio::net::TcpListener;
use tokio::signal::unix::{SignalKind, signal};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        // Color only for a person at a terminal, not a container's log store.
        .with_ansi(std::io::stdout().is_terminal())
        .init();
    let config = Config::from_env()?;
    let addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, config.port));
    let listener = TcpListener::bind(addr).await?;
    tracing::info!(%addr, "relay listening");
    let shutdown = CancellationToken::new();
    // In a container the relay is PID 1, which Linux sends no default
    // signal handling, so `docker stop`'s SIGTERM must be caught here or the
    // container is killed without closing its connections.
    let mut terminate = signal(SignalKind::terminate())?;
    tokio::spawn({
        let shutdown = shutdown.clone();
        async move {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = terminate.recv() => {}
            }
            tracing::info!("shutting down");
            shutdown.cancel();
        }
    });
    serve(listener, config, shutdown).await?;
    Ok(())
}
