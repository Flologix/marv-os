# Security policy

## Reporting a vulnerability

Please report vulnerabilities privately, through GitHub: open the Security tab of this repository and choose
**Report a vulnerability** (a private security advisory). Please do not open a public issue or pull request for one.

Include what you found, the version (Settings > About in the app, or `marv --version`), your operating system, and the
steps or a small project that reproduces it.

## What is in scope

Marv OS runs coding agents on your machine, so the interesting problems are about what an agent can reach or do
without you knowing:

- escapes from the command sandbox (bubblewrap on Linux, Seatbelt on macOS), such as reading your home folder, keys or
  Marv's own config, writing outside the project, or reaching host sockets;
- ways to get a tool call, file change or network request to run without the approval it should have needed;
- secrets (API keys, tokens, licence keys) showing up in logs, saved sessions, tool results or messages to a model;
- text from outside (a web page, an MCP server, another chat) being treated as instructions;
- the desktop app starting an agent with more access than it shows you.

Problems in third-party agents that Marv OS launches (Claude Code, Codex and so on) belong to those projects, unless
Marv OS gives them access it should not.

## What to expect

This is a small team. We aim to acknowledge a report within a few days and to tell you what we decide. We cannot
promise a fix date, and we do not run a bug bounty. If you want to be credited, say so in the report.

## How the sandbox works

[docs/SECURITY.md](docs/SECURITY.md) lists the security rules, how each one is enforced and the test that guards it.
