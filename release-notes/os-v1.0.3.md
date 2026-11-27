Gmail, Google Calendar and Google Drive now work fully in Marv's chats, and agents' terminals stay sharp when you switch between chats.

### Fixed

- **Gmail hid subjects and text from Marv**: Google holds back mail from outside your organisation until you allow
  the app to read it, and Marv couldn't show that question. Marv now asks you ("gmail asks: …", with Allow, Always
  and No) and passes your answer back to Google. The question is always yours to answer, even in yolo. The same
  goes for Google Calendar and Google Drive.
- **Google's errors are clearer**: when one of Google's APIs is off in your Cloud project, Marv says so in Google's
  own words, with the link to turn it on, instead of "Error POSTing to endpoint (HTTP 403)".
- **Switching between chats could scramble an agent's terminal**, with text drawn over other text or letters
  missing in words. Marv now resizes the terminal once its size settles, redraws it when it shows again, and asks
  Claude Code to redraw its screen (what ctrl+L did by hand).
- **An agent's terminal could be smaller than its pane**: Marv measured the characters before its faster renderer
  started, so the agent drew in fewer columns and rows than fit. It measures again once the renderer is on.

### Known issues

- **Claude Code chats still can't read Gmail fully**: Claude Code can't answer Google's question yet, so Google
  keeps holding back subjects and text there. Marv's own chats can.
- **macOS and Windows builds aren't signed yet**, so each asks once before the first open.

### Feedback

Please report anything that breaks, feels slow or confuses you in the
[issues](https://github.com/Flologix/marv-os/issues).
