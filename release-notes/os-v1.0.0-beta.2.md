The second beta of Marv OS. Use the ChatGPT or Kimi subscription you already pay for, teams coordinate better and
ask you less, sandboxed commands stop interrupting you, and agents check their own code after every edit in Rust and
Python projects.

### Use a subscription you already pay for

- **Continue with ChatGPT.** Setup can sign in with your ChatGPT Plus or Pro account, and Marv runs on its usage
  instead of an API key. Your browser opens to sign in; Marv tells you if the account has no subscription it can use.
- **Kimi Code.** Paste the API key from your Kimi Code membership (kimi.com/code) in setup or Settings, and pick
  `kimi-for-coding` or K3.
- **No surprise switching.** Marv never moves to another key or provider on its own. When your ChatGPT usage limit
  is reached, it stops and says so.

### Teams that coordinate better

- **The coordinator hands rework out as tasks.** When a member's report needs changes, the coordinator sends them back
  as new tasks instead of doing the work itself, and it's told to put dependent tasks in order.
- **Everyone knows who can do what.** The coordinator's brief lists what each member can do, and each chat's brief
  lists the files and folders its teammates may write. When a chat can't write a file, it's told once who can.
- **Every member takes the coordinator's tasks**, whatever its role. Run and verify roles may write the tool caches
  their role lists, so builds and tests work.
- **Workers ask you less.** A task from the coordinator runs edits and sandboxed commands inside the project without
  asking, unless its role sets narrower limits. A role with no edit tool and no files to write stays read-only.
- **Wake-ups have a limit you control.** Each chat in a team has a budget of wake-ups that refills only when you type,
  so a team can't keep waking itself forever. A wake-up that's dropped tells both the sender and you.
- **Starting a team is cleaner.** Starting a new team in a folder ends the previous team's rules there and says so,
  and the board uses the current team by default.
- **No messages to yourself.** A chat that tries to message its own inbox is told that a task's report goes back
  to the coordinator on its own.

### Fewer interruptions

- **Sandboxed commands run without asking.** A command that runs in the sandbox with no network no longer asks
  first. Commands that need the network, change git or touch protected paths still ask. To have them ask again, set
  `"approveSandboxed": "ask"` in `~/.marv/config.json` or run `/yolo off`; `/yolo` shows what's in effect. If you had
  already turned yolo off, nothing changes for you.
- **Woken turns are read-only.** When a message from a teammate wakes a chat, its sandboxed commands can read the
  project and build into `/tmp`, but anything that changes the project fails or asks you.
- **A chat's `/tmp` keeps its files between commands** (Linux), so agents stop redoing work. Each chat gets its own
  private `/tmp`, deleted with the chat.
- **Editing a file the agent hadn't read costs one step less.** The refusal now shows the file, so the next edit
  goes through.

### Agents check their own code

- **Rust and Python checks after edits.** In a Rust project Marv runs `cargo check`, and in a Python project
  basedpyright or pyright, in the sandbox after the agent's edits, and tells the agent about new errors only. Turn a
  language off with `/diagnostics rust off` (or `python`, `typescript`); `/diagnostics` shows each language's state.

### Smaller fixes

- **`/cost` shows each model's share** when a chat used more than one.
- **Chats started only by teammates' messages are saved**, titled "Woken by a message from …", and `marv -c`
  skips them so it continues the chat you last typed in.
- **Serving a folder is stricter.** It answers only `localhost`, never serves hidden folders such as `.git`, and runs
  with the project read-only.
- **macOS:** a sandboxed command can no longer read other processes' details, and stopping a command right after it
  starts now ends it reliably.

### Feedback

This is a beta. Please report anything that breaks, feels slow or confuses you in the
[issues](https://github.com/Flologix/marv-os-releases/issues).
