//! Agent access: an MCP server inside the app, so an external agent (Claude
//! Code, Codex, Claude Desktop) can run the operations the user runs from the
//! keyboard. The tools are thin wrappers over [`crate::ops`].
//!
//! **Off by default, and local only.** Nothing listens until the user turns
//! agent access on in Settings, and the listener binds `127.0.0.1`. Turning
//! it off stops the listener at once, even with clients connected; agent
//! turns and sends already under way keep running.
//!
//! **In process, not a second program.** A second process could not drive
//! the projects this app holds open (each project is locked to one process),
//! so the server runs inside the app and reaches the app state per request.
//!
//! **What is at stake.** A send runs an agent with permission prompts
//! disabled, so whoever can call `send` can run anything on the Mac. The
//! server's security work is making sure only a process the user configured
//! can call it: see [`server`] for the `Host`/`Origin` checks and [`token`]
//! for the bearer token.

pub mod app_nap;
pub mod config;
pub mod server;
pub mod token;
pub mod tools;

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use switchboard_prompts::SecretStore;

use crate::error::AppError;
use crate::ops::handle::AppStateHandle;

use config::AgentAccessConfig;
use server::Listener;

/// Agent access as Settings shows it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentAccessStatus {
    /// The user's setting: on means the app listens whenever it runs.
    pub enabled: bool,
    pub port: u16,
    /// Listening right now.
    pub listening: bool,
    /// Why it is on but not listening (the port is taken), or that it is off
    /// but the setting could not be saved, for Settings to show instead of
    /// failing.
    pub error: Option<String>,
    /// The token clients present; `None` until agent access is first turned
    /// on.
    pub token: Option<String>,
    /// The URL clients connect to.
    pub url: String,
}

/// The server's lifecycle: started at launch when the user left it on, and
/// from Settings.
pub struct AgentAccess {
    config_path: Option<PathBuf>,
    secrets: Arc<dyn SecretStore>,
    handle: Arc<dyn AppStateHandle>,
    home: PathBuf,
    /// Held across a start or stop, so two Settings actions cannot race the
    /// port.
    inner: tokio::sync::Mutex<Inner>,
}

struct Inner {
    config: AgentAccessConfig,
    listener: Option<Listener>,
    error: Option<String>,
}

impl AgentAccess {
    /// `config_path` is the user-global `config.yaml`, or `None` when no
    /// config directory resolved (the setting then lasts the session).
    pub fn new(
        config_path: Option<PathBuf>,
        secrets: Arc<dyn SecretStore>,
        handle: Arc<dyn AppStateHandle>,
        home: PathBuf,
    ) -> Self {
        let config = config_path.as_deref().map(config::load).unwrap_or_default();
        Self {
            config_path,
            secrets,
            handle,
            home,
            inner: tokio::sync::Mutex::new(Inner {
                config,
                listener: None,
                error: None,
            }),
        }
    }

    /// At launch: listen if the user left agent access on. A port in use
    /// shows in Settings; the app starts either way.
    pub async fn resume(&self) -> AgentAccessStatus {
        let mut inner = self.inner.lock().await;
        if inner.config.enabled && inner.listener.is_none() {
            self.listen(&mut inner).await;
        }
        self.report(&inner)
    }

    pub async fn status(&self) -> AgentAccessStatus {
        let inner = self.inner.lock().await;
        self.report(&inner)
    }

    /// Turn agent access on or off, and remember the choice. Turning it on
    /// creates the token the first time.
    ///
    /// **Off stops before it saves.** Off is the user's brake, so it must not
    /// depend on a disk write: the listener stops first, and a save that then
    /// fails is reported alongside the stopped state rather than as a failure.
    /// On saves first, so a failed save starts nothing.
    pub async fn set_enabled(&self, enabled: bool) -> Result<AgentAccessStatus, AppError> {
        let mut inner = self.inner.lock().await;
        tracing::info!(enabled, "agent access setting changed");
        if !enabled {
            Self::stop(&mut inner).await;
            inner.config.enabled = false;
            if let Err(error) = self.persist(inner.config) {
                tracing::warn!(%error, "agent access is off but the setting was not saved");
                inner.error = Some(format!(
                    "Agent access is off, but the setting couldn't be saved ({error}), so it may turn back on the next time Switchboard starts."
                ));
            }
            return Ok(self.report(&inner));
        }
        token::load_or_create(self.secrets.as_ref())?;
        self.persist(AgentAccessConfig {
            enabled: true,
            ..inner.config
        })?;
        inner.config.enabled = true;
        Self::stop(&mut inner).await;
        self.listen(&mut inner).await;
        Ok(self.report(&inner))
    }

    /// Move to another port, restarting the listener if it is on.
    pub async fn set_port(&self, port: u16) -> Result<AgentAccessStatus, AppError> {
        if port == 0 {
            return Err(AppError::InvalidArgument(
                "choose a port from 1 to 65535".to_owned(),
            ));
        }
        let mut inner = self.inner.lock().await;
        self.persist(AgentAccessConfig {
            port,
            ..inner.config
        })?;
        inner.config.port = port;
        tracing::info!(port, "agent access port changed");
        Self::stop(&mut inner).await;
        if inner.config.enabled {
            self.listen(&mut inner).await;
        }
        Ok(self.report(&inner))
    }

    /// Replace the token. A running listener restarts, which ends every open
    /// session, so every client is cut off at once rather than at its next
    /// request.
    pub async fn regenerate_token(&self) -> Result<AgentAccessStatus, AppError> {
        let mut inner = self.inner.lock().await;
        token::regenerate(self.secrets.as_ref())?;
        tracing::info!("agent access token regenerated");
        if inner.listener.is_some() {
            Self::stop(&mut inner).await;
            self.listen(&mut inner).await;
        }
        Ok(self.report(&inner))
    }

    /// Stop listening without changing the setting, for the app's quit.
    pub async fn shut_down(&self) {
        let mut inner = self.inner.lock().await;
        Self::stop(&mut inner).await;
    }

    async fn listen(&self, inner: &mut Inner) {
        let port = inner.config.port;
        let token = match token::load_or_create(self.secrets.as_ref()) {
            Ok(token) => token,
            Err(error) => {
                inner.error = Some(error.to_string());
                return;
            }
        };
        match server::start(port, token, Arc::clone(&self.handle), self.home.clone()).await {
            Ok(listener) => {
                inner.listener = Some(listener);
                inner.error = None;
            }
            Err(error) => {
                tracing::warn!(port, %error, "agent access could not listen");
                inner.error = Some(if error.kind() == std::io::ErrorKind::AddrInUse {
                    format!(
                        "Port {port} is already in use by another program. Choose a different port."
                    )
                } else {
                    format!("Could not listen on port {port}: {error}")
                });
            }
        }
    }

    async fn stop(inner: &mut Inner) {
        inner.error = None;
        if let Some(listener) = inner.listener.take() {
            listener.stop().await;
        }
    }

    fn persist(&self, config: AgentAccessConfig) -> Result<(), AppError> {
        match &self.config_path {
            Some(path) => config::save(path, config),
            None => Ok(()),
        }
    }

    fn report(&self, inner: &Inner) -> AgentAccessStatus {
        let port = inner
            .listener
            .as_ref()
            .map_or(inner.config.port, Listener::port);
        AgentAccessStatus {
            enabled: inner.config.enabled,
            port,
            listening: inner.listener.is_some(),
            error: inner.error.clone(),
            token: token::load(self.secrets.as_ref()).ok().flatten(),
            url: format!("http://127.0.0.1:{port}/mcp"),
        }
    }
}
