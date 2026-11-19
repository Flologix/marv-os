The first release candidate of Marv OS 1.0, and the first built from the open-source code
([Flologix/marv-os](https://github.com/Flologix/marv-os), Apache-2.0). Every chat is now its agent's own terminal, so
Claude Code, Codex and about fifteen other coding agents run in Marv OS next to Marv and work together in teams.
Marv OS now runs on Windows too.

### Your coding agents, in one place

- **Every chat is its agent's own terminal.** Pick Marv, Claude Code, Codex, OpenCode, Gemini CLI, Pi, Cursor Agent,
  Crush, Antigravity, Hermes, Amp, Droid, Copilot CLI, Kimi CLI, Qwen Code, Cline, Kiro, Letta Code, Qoder or Grok CLI,
  or bring your own agent by its command. A plain Terminal chat is there too. Each agent runs on its own login.
- **One set of permission modes for every agent:** Supervised, Auto-accept edits, Auto and Full access, or the
  agent's own settings.
- **Each turn is kept**, with its own diff and a restore to before it.
- **Switch a chat to another agent** with the conversation handed over, or **delegate a task** to another agent and
  wait for its reply.
- **Every chat shows its state** (working, needs you, idle) as a tiny Marv, and a right-click renames a chat.
- **Activity shows each agent's usage** from its own history (Claude Code, Codex, OpenCode).
- **Apps you sign in to** (Gmail, Outlook, Slack and more) reach Claude Code, Codex, OpenCode and Crush as well as Marv.

### Teams of any agents

- **Every installed agent can be a team member**, and team mail is typed into its terminal.
- **Worktree teams:** each member can work on its own git branch. Its commits and merges run in the sandbox without
  asking, with the repository's hooks and config read-only. The lead hands commits and merges to a worker instead of
  asking you to run git.
- **The Teams screen shows a worktree team's posts**, and the team cards show who is waiting and who is working.
- **Deleting a chat removes its worktree and branch** only when nothing would be lost. Otherwise Marv OS keeps them and
  says why.
- **Merge or Discard** a worktree chat's branch from the chat.

### Windows

- **Marv OS runs on Windows x64**, with an installer. Agents run in Microsoft's ConPTY, Marv's bash is Git Bash, and
  agents installed with winget or npm are found.
- **No command sandbox on Windows yet:** each change is asked first. An AppContainer sandbox is in testing
  (`MARV_APPCONTAINER=1`).

### A Marv account (free)

- **Teams and Apps need a free Marv account**: Google, GitHub or email sign-in, in the first run (you can skip it) or
  under Settings > Account. Everything else works without one.

### Security fixes

- **The sandbox hides your home folder whatever `HOME` says.** Before, a changed or unset `HOME` left the real home
  readable to sandboxed commands
  ([GHSA-5hg5-h939-2jcm](https://github.com/Flologix/marv/security/advisories/GHSA-5hg5-h939-2jcm)).
- **`/run` folders on `PATH` are judged by where they really lead**, so a symlinked entry can't reopen the host's
  sockets ([GHSA-xrwc-rj25-jp46](https://github.com/Flologix/marv/security/advisories/GHSA-xrwc-rj25-jp46)).

### Smaller changes

- **Paste or name a screenshot**, and agents can look at what they build.
- **Settings are simpler**, grouped like a phone's, and Settings > About has Report a problem, looked into by your own
  coding agent.
- **lazygit in the Git tab**, bundled.
- **Closing a terminal ends its agent's whole session**, and quitting Marv OS ends the agents.
- **fnm's Node versions show in the sandbox again.**
- **macOS:** Marv and lazygit are bundled inside the app.

### Feedback

This is a release candidate: if nothing serious turns up, it becomes 1.0.0. Please report anything that breaks, feels
slow or confuses you in the [issues](https://github.com/Flologix/marv-os/issues).
