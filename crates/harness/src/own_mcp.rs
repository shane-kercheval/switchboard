//! Switchboard's own MCP server, as an agent's harness config might name it.
//!
//! A user who registers Switchboard's server with their harness (so that a
//! Claude Code or Codex session they run by hand can direct Switchboard) has
//! also registered it for every agent Switchboard starts, because those agents
//! read the same config files. An agent that can send to its siblings, start
//! workflows, and wait on itself is a loop the user did not set up, so the
//! Claude Code and Codex adapters turn the server off for each run. This is a
//! guard against accidents, not a security boundary: an agent with a shell can
//! still reach the port.

/// The server name the setup instructions tell users to register.
pub const OWN_MCP_SERVER_NAME: &str = "switchboard";

/// The loopback host names Switchboard's server accepts in a `Host` header, so
/// the two forms a config entry can use to reach it.
pub const OWN_MCP_SERVER_HOSTS: [&str; 2] = ["127.0.0.1", "localhost"];

/// This Switchboard instance's MCP server. Adapters turn off any configured
/// server named [`OWN_MCP_SERVER_NAME`] or whose URL points at this port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwnMcpServer {
    pub port: u16,
}

impl OwnMcpServer {
    #[must_use]
    pub fn new(port: u16) -> Self {
        Self { port }
    }

    /// One wildcard URL pattern per host form, in the shape Claude Code's
    /// `deniedMcpServers[].serverUrl` takes.
    #[must_use]
    pub fn url_patterns(self) -> Vec<String> {
        OWN_MCP_SERVER_HOSTS
            .iter()
            .map(|host| format!("http://{host}:{}/*", self.port))
            .collect()
    }

    /// Whether `url` addresses this server: `http`, either loopback host form
    /// (any letter case), this port, any path.
    #[must_use]
    pub fn is_own_url(self, url: &str) -> bool {
        let Some(scheme_end) = url.find("://") else {
            return false;
        };
        if !url[..scheme_end].eq_ignore_ascii_case("http") {
            return false;
        }
        let rest = &url[scheme_end + 3..];
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        let Some((host, port)) = authority.rsplit_once(':') else {
            return false;
        };
        port == self.port.to_string()
            && OWN_MCP_SERVER_HOSTS
                .iter()
                .any(|own| host.eq_ignore_ascii_case(own))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_patterns_cover_both_host_forms() {
        assert_eq!(
            OwnMcpServer::new(47821).url_patterns(),
            vec![
                "http://127.0.0.1:47821/*".to_owned(),
                "http://localhost:47821/*".to_owned(),
            ]
        );
    }

    #[test]
    fn is_own_url_matches_either_host_on_this_port() {
        let own = OwnMcpServer::new(47821);
        for url in [
            "http://127.0.0.1:47821/mcp",
            "http://localhost:47821/mcp",
            "HTTP://LocalHost:47821/mcp",
            "http://127.0.0.1:47821",
            "http://127.0.0.1:47821?x=1",
        ] {
            assert!(own.is_own_url(url), "{url}");
        }
    }

    #[test]
    fn is_own_url_rejects_other_ports_hosts_and_schemes() {
        let own = OwnMcpServer::new(47821);
        for url in [
            "http://127.0.0.1:47822/mcp",
            "http://127.0.0.1:478210/mcp",
            "http://127.0.0.2:47821/mcp",
            "http://example.com:47821/mcp",
            "https://127.0.0.1:47821/mcp",
            "http://127.0.0.1/mcp",
            "127.0.0.1:47821/mcp",
            "",
        ] {
            assert!(!own.is_own_url(url), "{url}");
        }
    }
}
