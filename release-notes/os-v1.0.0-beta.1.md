The first public beta of Marv OS: a desktop for running several coding agents on your own machine, seeing what
each one is doing, and stepping in only when something needs you.

### Run many agents at once

- **Every chat is its own process.** Start as many as you like, each in its folder, each with its own model.
- **Teams.** Put chats under a rule and they can message each other, wake up when mail arrives, read each other's
  chats (you see a notice when they do) and share a board to claim, note and finish tasks.
- **Roles.** Give a chat a role when it starts, such as coordinator, worker or one you write in `.marv/roles/`. A role
  sets which tools the chat may use, which files it may write and whose tasks it takes. A task from a teammate runs
  edits inside the role's files without asking; anything else still asks you in that chat.
- **Team setup.** The New team dialog starts several chats at once, each with its role and the files it may write,
  and can save the setup as `.marv/team.md` to reuse. The team view shows each member's latest report, and End team
  removes everything the team was granted.
- **Separate worktrees.** Chats working unattended get their own git worktree by default, and the desktop shows
  when two chats edit the same files.

### Work while you're away

- **Background grants** let a chat keep going without you: safe actions only, a daily spend cap (default $2) and a
  30-minute limit per turn. Anything that needs your approval waits for you on Home.
- **Timers** such as "every morning at 9" are set only by you. A granted chat can schedule its own next turn,
  within limits.

### Stay in control

- **Budgets** in dollars for everything, each folder, each chat and each grant, set only by you. A turn you started
  asks at the limit; an unattended one pauses and tells you.
- **Pause, resume and stop** any chat or a whole team. Chats can send each other mail, never commands.
- **Permissions** in one place: every rule, grant, trusted project and budget, each one revocable.

### Safe by default

- Text from the web, GitHub, MCP servers and other chats is treated as data, never as instructions, and a chat that
  has read it asks before reaching the network.
- A project's own instructions count only after you trust them, and again whenever they change.
- Edits refuse to overwrite changes the agent hasn't seen, whether you made them or another chat did.
- Approvals show what a call acts on (the file path or the address) first. After a chat has read untrusted text,
  its approvals say so, and an MCP call after that point is asked about every time.
- Invisible control characters, such as the ones that reverse text direction, are removed from MCP server output,
  tool names and approvals.

### The desktop

- **Home:** what needs you, what happened while you were away, and today's spend.
- **Folders:** your chats by folder, with a dock for files, changes, the team board, a terminal and the context
  window.
- **Teams, Activity and Apps:** rules and timers; a task manager for agents (state, context used, cost per hour,
  child processes); MCP servers and permissions.
- **Light and dark themes**, following your desktop.
- **Links open in your browser.** A link in a chat, such as a local server an agent started, no longer replaces the
  app's window.
- **A hang watchdog.** If the window stops responding, Marv OS writes a local report and reloads the page without
  touching your running chats. Launching it again closes a window that's truly stuck instead of waiting on it.

### From the terminal

The `marv` CLI that ships with Marv OS adds `marv kernel ps`, `log`, `usage` and `budget` to see the same agents,
history and spend from a shell.

### Feedback

This is a beta. Please report anything that breaks, feels slow or confuses you in the
[issues](https://github.com/Flologix/marv-os-releases/issues). Hang reports are saved on your machine only, in
`~/.local/share/ai.flologix.marv/reports/`; attach one if the window froze.
