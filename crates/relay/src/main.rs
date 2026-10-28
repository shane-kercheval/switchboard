// A panic in one connection's task must end only that connection, which
// needs unwinding; the workspace's release profile aborts, for the desktop.
#[cfg(panic = "abort")]
compile_error!(
    "the relay must unwind on panic: build it with `make relay-image` or `--profile relay`"
);

use std::backtrace::{Backtrace, BacktraceStatus};
use std::io::IsTerminal;
use std::net::{Ipv4Addr, SocketAddr};
use std::panic::PanicHookInfo;

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
    std::panic::set_hook(Box::new(log_panic));
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

/// Logs a panic where the rest of the relay's log goes, rather than to
/// stderr, with its backtrace when `RUST_BACKTRACE` asks for one. It must
/// not panic itself.
fn log_panic(info: &PanicHookInfo<'_>) {
    let message = info.payload_as_str().unwrap_or("a panic with no message");
    let location = info.location().map(ToString::to_string);
    let backtrace = Backtrace::capture();
    if backtrace.status() == BacktraceStatus::Captured {
        tracing::error!(location, %backtrace, "panicked: {message}");
    } else {
        tracing::error!(location, "panicked: {message}");
    }
}
