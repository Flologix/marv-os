# Gmail, Google Calendar and Google Drive

Marv OS connects to Google's own MCP servers for Gmail, Calendar and Drive. Google doesn't let apps sign in to these
servers with a shared client, so you create a small OAuth client in your own Google Cloud project once. It takes about
ten minutes, and Calendar and Drive reuse the same client.

You need a Google account and access to [Google Cloud Console](https://console.cloud.google.com). It's free; no
billing account is needed.

> Google's MCP servers are in preview. If a step below doesn't match what you see, or the APIs aren't listed, your
> project may need to join the [Workspace Developer Preview](https://developers.google.com/workspace/preview).

## 1. Pick or create a project

In Cloud Console, use the project picker at the top › **New project**. Any name works (for example `marv`).

## 2. Turn on the APIs

Open **APIs & Services › Library**, search for each API and click **Enable**:

| For        | Enable both                                  |
| ---------- | -------------------------------------------- |
| Gmail      | **Gmail MCP API** and **Gmail API**          |
| Calendar   | **Calendar MCP API** and **Google Calendar API** |
| Drive      | **Drive MCP API** and **Google Drive API**   |

The MCP API is a separate switch from the regular API. If it's off, signing in still works, but every request fails
with HTTP 403 and "Gmail MCP API has not been used in project … or it is disabled". After enabling, allow a few minutes
before trying again.

## 3. Set up the consent screen

Open **APIs & Services › OAuth consent screen** (shown as **Google Auth Platform** in newer consoles) and click
**Get started**.

- **App name and support email:** anything; only you see them.
- **Audience:**
  - **Google Workspace account** (your company's domain): choose **Internal**. Anyone in your organization can then
    sign in, and nothing else is needed.
  - **Personal @gmail.com account:** choose **External** and leave the app in **Testing**. Then, under **Audience ›
    Test users**, add your own Gmail address. Only test users can sign in.
- **Data access** (scopes): optional, since Marv asks for its scopes at sign-in. You can add them here to see them
  listed:
  - Gmail: `gmail.readonly`, `gmail.compose`
  - Calendar: `calendar.readonly`, `calendar.events`
  - Drive: `drive.readonly`, `drive.file`

## 4. Create the OAuth client

Open **APIs & Services › Credentials › Create credentials › OAuth client ID**.

- **Application type: Desktop app.** A Desktop client accepts Marv's sign-in on your own computer at any local port,
  so there are no redirect URIs to enter.
- Click **Create**, then copy the **Client ID** and **Client secret**.

If you use a **Web application** client instead, add both redirect URIs, one for Marv and one for Claude Code:

```
http://127.0.0.1:53682/callback
http://localhost:53682/callback
```

## 5. Connect it in Marv OS

1. Open **Apps › Gmail** (or Calendar, or Drive).
2. Paste the **Client ID** and **Client secret**, then click **Sign in with Google**.
3. Your browser opens Google's page. Pick your account and approve.
   - With a personal account in Testing, Google shows **"Google hasn't verified this app"**. That's your own app:
     click **Continue**.
4. The app's row shows **Signed in**. Start a new chat and ask, for example, "what are my top 10 emails?"

The client is kept in `~/.marv/mcp.json` and the login in `~/.marv/mcp-tokens.json`, both on your computer only.
Adding Calendar or Drive later offers to reuse the same Google client, so you only sign in.

## When Google asks first

Before it hands over mail from outside your organisation (or makes some changes), Google asks you. Marv shows that
question in the chat as **"gmail asks: …"**:

- **Allow**: this time only.
- **Always**: Google stops asking for this tool (it remembers, not Marv).
- **No**: the agent is told you didn't allow it.

The question is always yours to answer, even in yolo. Trashing mail asks every time. This needs Marv OS 1.0.3 or
later; before it, Google held back the subject and text of those emails.

## Claude Code chats

Claude Code chats in Marv OS see the same apps, and Marv hands Claude Code the client secret. Claude Code keeps its own
login, though, so sign in once there too: in a Claude Code chat, type `/mcp`, pick **gmail**, then **Authenticate**,
and approve in the browser.

Claude Code can't answer Google's question yet, so there Google keeps holding back the subject and text of mail from
outside your organisation. Marv's own chats read it.

## When something goes wrong

| What you see | What to do |
| ------------ | ---------- |
| HTTP 403, "Gmail MCP API has not been used in project … or it is disabled" | Enable the Gmail MCP API (step 2), wait a few minutes, start a new chat. |
| `Error 403: access_denied`, "has not completed the Google verification process" | Add your address under **Audience › Test users** (step 3). |
| `Error 400: redirect_uri_mismatch` | The client is a Web application: add both redirect URIs (step 4), or create a Desktop app client. |
| `admin_policy_enforced` or "blocked by your administrator" | Your Workspace admin restricts third-party apps: ask them to allow your client ID, or to set it up as Internal. |
| Senders and dates only: "withheld under its data-protection policy" | Update to Marv OS 1.0.3 or later and ask in a Marv chat; allow when Google asks. |
| Claude Code says `client_secret is missing` | Update to Marv OS 1.0.2 or later, then open a new Claude Code chat. |
| Gmail worked, then asks to sign in again after a week | Google expires logins for External apps in **Testing** after 7 days. Sign in again, or move the app to **In production** (Audience › Publish app). An unverified app in production still shows the warning screen but keeps the login. |
