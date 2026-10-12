# Security Policy

Please report vulnerabilities **privately** through GitHub Security Advisories ("Report a
vulnerability") on this repository. Do not open public issues for security problems.

okbot forwards untrusted chat messages to AI agents, so these areas matter most:

- an agent gaining tools its room's policy does not allow (shell, file access);
- scope bypass: reading another user's or room's memory through the MCP tools;
- leaking secrets (platform tokens, run tokens) into logs, prompts or chat replies;
- webhook authentication (Zalo secret header, Google Chat JWT);
- path traversal in the memory writer.

We aim to acknowledge reports within 7 days. Supported versions: the latest 0.x release until 1.0.
