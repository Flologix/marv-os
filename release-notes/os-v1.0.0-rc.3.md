The third release candidate of Marv OS 1.0. It fixes Marv chats on a clean Windows install, stops asking for your key
twice, and fixes six problems found in team runs.

### Windows

- **Marv chats start on a clean Windows.** In rc.2, Marv's own chats ended at once on a PC without Microsoft's Visual
  C++ runtime. The runtime is now built in, so nothing extra needs installing.
- **Your key from the first run reaches Marv.** A new Marv chat starts on the provider and key you chose when setting
  up Marv OS, instead of running Marv's own setup and asking again. An existing Marv setup is never changed.

### Teams

- **Stop team ends its members' agents**, each with everything it started. Before, it only paused the chats and their
  agents kept running. Resume team still works.
- **Deleting a chat removes a worker's branch that's merged elsewhere.** A branch whose commits another branch already
  holds (one worker merged another's) goes with its chat. A branch with commits only it has, or with uncommitted files,
  is still kept.

### Smaller fixes

- **A chat whose folder is gone can be deleted.** It shows under the nearest folder with a "folder gone" badge.
- **Quitting from outside quits properly** (Linux): stopping Marv OS's launcher, for example from a script or a session
  manager, now quits the app and its agents instead of leaving them running.
- **Activity shows a Marv chat's CPU and memory** again, instead of "—".
- **Your own agents show by their name**, not their id.
- **The bundled `marv` reports its real version** (0.16.5, with 0.16.5's sandbox fixes), so it no longer says a newer
  Marv is out.

### Known issues

- **A new Marv chat uses Marv's default model** the first time, not the one you picked when setting up. Pick the model
  in the chat (or with `/model`) to change it.

### Feedback

This is a release candidate: if nothing serious turns up, it becomes 1.0.0. Please report anything that breaks, feels
slow or confuses you in the [issues](https://github.com/Flologix/marv-os/issues).
