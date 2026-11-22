Marv OS 1.0: a desktop for running your coding agents side by side on your own machine. Every chat is its agent's
own terminal, so Marv, Claude Code, Codex, OpenCode, Gemini CLI and about fifteen other agents work next to each other,
in teams if you like, while you see what each one is doing and step in only when something needs you. Free and open
source ([Flologix/marv-os](https://github.com/Flologix/marv-os), Apache-2.0), on Linux, macOS and Windows.

### What's in 1.0

- **Your agents, in one place.** Pick Marv, Claude Code, Codex, OpenCode, Gemini CLI, Pi, Cursor Agent, Crush,
  Antigravity, Hermes, Amp, Droid, Copilot CLI, Kimi CLI, Qwen Code, Cline, Kiro, Letta Code, Qoder or Grok CLI, or
  add your own by its command. Each runs on its own login.
- **One set of permission modes for every agent:** Supervised, Auto-accept edits, Auto and Full access.
- **Each turn is kept** with its own diff and a restore to before it. Switch a chat to another agent with the
  conversation handed over, or delegate a task to another agent and wait for its reply.
- **Teams of any agents.** Members message each other and share a board. Each member can work on its own git branch;
  its commits run in the sandbox without asking, and the lead hands merging to a worker.
- **A sandbox for Marv's commands** on Linux (bubblewrap) and macOS (Seatbelt). Windows has no command sandbox yet, so
  each change is asked first.
- **Activity** shows every agent's state, CPU, memory and spend, plus each agent's usage from its own history.
- **Apps you sign in to** (Gmail, Outlook, Slack and more) reach your agents. Teams and Apps need a free Marv account;
  everything else works without one.

### Since 1.0.0-rc.3

- **Linux AppImage: agents' Python and other tools work.** The AppImage's own library and Python settings no longer
  leak into the agents and commands it starts, so `python3` in an agent no longer fails with "No module named
  'encodings'".
- **The first Marv chat starts on the model you picked in setup**, not Marv's default.
- **Changing a running agent's permission mode says it applies when the agent starts again**, with a button to
  restart it on its conversation. Before, the agent kept its old mode without saying so.
- **Opening Marv OS again shows the window that's already open** instead of starting a second copy.
- **New files show up in the files pane** as soon as an agent finishes its turn or you come back to the window.
- **Without Git, errors name Git** and link to Git for Windows, instead of "program not found".
- **Activity shows CPU and memory for Marv chats**, on Windows too (before, a Marv chat's numbers were missing).
- **Security:** rustls 0.23.46.

### Known issues

- **macOS and Windows builds aren't signed yet**, so each asks once before the first open.

### Feedback

Please report anything that breaks, feels slow or confuses you in the
[issues](https://github.com/Flologix/marv-os/issues).
