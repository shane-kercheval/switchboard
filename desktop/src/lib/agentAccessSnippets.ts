/// Ready-to-paste configuration for each client of agent access, built from
/// the server's URL and token. The shapes are the ones measured against each
/// client (`docs/research/mcp-client-behavior.md`).

/// The environment variable the Codex configuration reads the token from, so
/// the token stays out of the folder's config file.
export const CODEX_TOKEN_VARIABLE = "SWITCHBOARD_TOKEN";

/// The `mcp-remote` release Claude Desktop was measured with. Pinned so an
/// update to the bridge cannot change behavior under the user.
export const MCP_REMOTE_VERSION = "0.14.3";

export type AgentAccessSnippets = {
  claudeCode: string;
  codexConfig: string;
  codexEnvironment: string;
  claudeDesktop: string;
};

export function agentAccessSnippets(url: string, token: string): AgentAccessSnippets {
  // Default (local) scope: the token goes into the user's private
  // `~/.claude.json`, never into a project's committed `.mcp.json`.
  const claudeCode = `claude mcp add --transport http switchboard ${url} --header "Authorization: Bearer ${token}"`;
  const codexConfig = [
    "[mcp_servers.switchboard]",
    `url = "${url}"`,
    `bearer_token_env_var = "${CODEX_TOKEN_VARIABLE}"`,
    // Without it, unattended `codex exec` refuses every call.
    'default_tools_approval_mode = "approve"',
  ].join("\n");
  const codexEnvironment = `export ${CODEX_TOKEN_VARIABLE}=${token}`;
  const claudeDesktop = JSON.stringify(
    {
      mcpServers: {
        switchboard: {
          command: "/absolute/path/to/npx",
          // The header goes through `env` because Claude Desktop does not
          // escape the space in "Bearer <token>" inside `args`.
          args: [
            "-y",
            `mcp-remote@${MCP_REMOTE_VERSION}`,
            url,
            "--header",
            "Authorization:${AUTH_HEADER}",
          ],
          env: {
            AUTH_HEADER: `Bearer ${token}`,
            PATH: "/absolute/path/to/node/bin:/usr/bin:/bin",
          },
        },
      },
    },
    null,
    2,
  );
  return { claudeCode, codexConfig, codexEnvironment, claudeDesktop };
}
