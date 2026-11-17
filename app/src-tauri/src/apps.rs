//! Apps you sign in to (Gmail, Outlook, Slack, Notion…) in the Apps place. Each is a remote MCP server in
//! ~/.marv/mcp.json that logs in with OAuth: the person clicks Sign in, their browser opens the provider's own
//! consent page, and the tokens land in ~/.marv/mcp-tokens.json, the store every chat, `marv` and `marv -p` read.
//!
//! - Signing in needs no open chat: it runs here, on a manager of its own holding just that server, through the same
//!   login code as `/mcp login` (PKCE, a 127.0.0.1 redirect, refresh on a 401).
//! - Servers that register clients themselves (Notion, Linear) need nothing typed. Google, Microsoft and Slack only
//!   take a client registered ahead: Marv's own (`oauth.clientId` in the catalog) or the person's organization's,
//!   typed once. Its secret goes into mcp.json (0600, hidden from Marv's sandbox) and never back to the page: a second
//!   Google app reuses the first one's client by name.

use std::path::Path;
use std::sync::{Arc, Mutex};

use marv_mcp::config::{Include, McpServerConfig, McpTransport, ServerSource, load_mcp_config};
use marv_mcp::manager::{CONNECT_TIMEOUT, LOGIN_TIMEOUT, ManagerOptions, McpManager, login_status, own_authorization};
use marv_mcp::oauth::Authorize;
use marv_mcp::oauth_store::{McpTokenStore, login_key};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// One of your servers, as the Apps place shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppStatus {
    pub name: String,
    /// "oauth" (signs in), "key" (sends its own Authorization header), or "local" (a process on this machine).
    pub kind: &'static str,
    pub signed_in: bool,
    /// Where it signs in ("accounts.google.com"), once it has.
    pub host: Option<String>,
    /// Its own OAuth client (not one it registered): another app of the same provider can reuse it.
    pub has_client: bool,
}

/// What the page sends to connect an app: its address and how it signs in. No secret comes back out.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthSetup {
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub scope: Option<String>,
    pub callback_port: Option<u16>,
}

fn tokens_store(config_dir: &Path) -> McpTokenStore {
    McpTokenStore::new(config_dir.join("mcp-tokens.json"))
}

fn personal_servers(config_dir: &Path, home: &Path) -> Vec<McpServerConfig> {
    let env: marv_mcp::Env = std::env::vars().collect();
    load_mcp_config(home, config_dir, &env, Include { personal: true, project: false }).servers
}

/// Your servers (~/.marv/mcp.json), each with whether it is signed in.
pub fn statuses(config_dir: &Path, home: &Path) -> Vec<AppStatus> {
    let store = tokens_store(config_dir);
    personal_servers(config_dir, home).into_iter().map(|s| status_of(&s, &store)).collect()
}

fn status_of(server: &McpServerConfig, store: &McpTokenStore) -> AppStatus {
    let (kind, login, has_client) = match &server.transport {
        McpTransport::Stdio { .. } => ("local", None, false),
        t if own_authorization(t) => ("key", None, false),
        McpTransport::Http { url, oauth, .. } => (
            "oauth",
            Some(login_status(store.get(&login_key(ServerSource::Personal, url, "")).as_ref())),
            oauth.as_ref().is_some_and(|o| o.client_id.is_some()),
        ),
    };
    AppStatus {
        name: server.name.clone(),
        kind,
        signed_in: login.as_ref().is_some_and(|l| l.logged_in),
        host: login.and_then(|l| l.host),
        has_client,
    }
}

/// The mcp.json entry for a remote app that signs in. `reused` is another entry's `oauth` object (its client),
/// taken whole when the page asks to reuse it; the setup's scope and port still apply.
pub fn entry(url: &str, setup: &OAuthSetup, reused: Option<&Map<String, Value>>) -> Result<Value, String> {
    let url = url.trim();
    let parsed = url::Url::parse(url).map_err(|_| format!("{url} isn't a web address."))?;
    if parsed.scheme() != "https" {
        return Err("An app's address must start with https://.".into());
    }
    let clean = |v: &Option<String>| v.as_deref().map(str::trim).filter(|v| !v.is_empty()).map(String::from);
    let mut oauth = Map::new();
    if let Some(r) = reused {
        for key in ["clientId", "clientSecret"] {
            if let Some(v) = r.get(key) {
                oauth.insert(key.into(), v.clone());
            }
        }
        if !oauth.contains_key("clientId") {
            return Err("That app has no client of its own to reuse.".into());
        }
    } else {
        if let Some(id) = clean(&setup.client_id) {
            oauth.insert("clientId".into(), id.into());
        }
        if let Some(secret) = clean(&setup.client_secret) {
            if !oauth.contains_key("clientId") {
                return Err("A client secret needs its client ID.".into());
            }
            oauth.insert("clientSecret".into(), secret.into());
        }
    }
    if let Some(scope) = clean(&setup.scope) {
        oauth.insert("scope".into(), scope.into());
    }
    if let Some(port) = setup.callback_port {
        oauth.insert("callbackPort".into(), port.into());
    }
    let mut entry = json!({ "type": "http", "url": url });
    if !oauth.is_empty() {
        entry["oauth"] = Value::Object(oauth);
    }
    Ok(entry)
}

/// The `oauth` object of your server `name`, as written (its secret included: it stays on this side).
pub fn oauth_of(config_dir: &Path, name: &str) -> Result<Map<String, Value>, String> {
    let file = marv_mcp::personal_file::read(&marv_mcp::personal_file::personal_file(config_dir))?;
    let file = serde_json::to_value(marv_mcp::ordered::Ordered::Object(file)).map_err(|e| e.to_string())?;
    file.pointer(&format!("/mcpServers/{}/oauth", name.replace('~', "~0").replace('/', "~1")))
        .and_then(Value::as_object)
        .cloned()
        .ok_or_else(|| format!("{name} has no OAuth client to reuse."))
}

/// The sign-in running now: closing its manager stops it (Cancel, or another sign-in starting).
static RUNNING: Mutex<Option<Arc<McpManager>>> = Mutex::new(None);

fn manager_for(server: McpServerConfig, config_dir: &Path, home: &Path) -> Arc<McpManager> {
    McpManager::new(
        vec![server],
        ManagerOptions {
            root: home.to_path_buf(),
            project_key: String::new(),
            home: home.to_path_buf(),
            version: env!("CARGO_PKG_VERSION").into(),
            // Yours only: a folder's servers are trusted per folder, in a chat.
            trust: None,
            logins: Some(tokens_store(config_dir)),
            env: std::env::vars().collect(),
            connect_timeout: CONNECT_TIMEOUT,
            login_timeout: LOGIN_TIMEOUT,
        },
    )
}

/// Signs in to your server `name`: `authorize` opens the provider's page in the person's browser. Answers how many
/// tools it offers once signed in. One at a time: a second sign-in stops the first.
pub async fn sign_in(config_dir: &Path, home: &Path, name: &str, authorize: Authorize) -> Result<usize, String> {
    let server = personal_servers(config_dir, home)
        .into_iter()
        .find(|s| s.name == name)
        .ok_or_else(|| format!("There's no app named \"{name}\" in ~/.marv/mcp.json."))?;
    if !matches!(server.transport, McpTransport::Http { .. }) || own_authorization(&server.transport) {
        return Err(format!("{name} doesn't sign in: it runs with what its setup gave it."));
    }
    let mcp = manager_for(server, config_dir, home);
    if let Some(previous) = RUNNING.lock().unwrap_or_else(|e| e.into_inner()).replace(mcp.clone()) {
        tokio::spawn(async move { previous.close().await });
    }
    let starting = mcp.clone();
    tokio::spawn(async move { starting.start().await });
    let result = match mcp.login(name, authorize) {
        Ok(login) => match login.done.await {
            Ok(Ok(status)) => Ok(status.tools.len()),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err("The sign-in stopped.".into()),
        },
        Err(e) => Err(e.to_string()),
    };
    {
        let mut running = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
        if running.as_ref().is_some_and(|m| Arc::ptr_eq(m, &mcp)) {
            *running = None;
        }
    }
    mcp.close().await;
    result.map_err(|e| marv_core::printable(&e))
}

/// Stops the sign-in waiting for the browser, if any.
pub async fn cancel_sign_in() {
    let running = RUNNING.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(mcp) = running {
        mcp.close().await;
    }
}

/// Signs out of your server `name`: revokes where the provider can, and forgets the tokens.
pub async fn sign_out(config_dir: &Path, home: &Path, name: &str) -> Result<(), String> {
    let server = personal_servers(config_dir, home)
        .into_iter()
        .find(|s| s.name == name)
        .ok_or_else(|| format!("There's no app named \"{name}\" in ~/.marv/mcp.json."))?;
    let mcp = manager_for(server, config_dir, home);
    let starting = mcp.clone();
    tokio::spawn(async move { starting.start().await });
    let result = mcp.logout(name).await.map(|_| ()).map_err(|e| marv_core::printable(&e.to_string()));
    mcp.close().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_carries_only_what_was_given() {
        let plain = entry("https://mcp.notion.com/mcp", &OAuthSetup::default(), None).unwrap();
        assert_eq!(plain, json!({"type": "http", "url": "https://mcp.notion.com/mcp"}));
        let setup = OAuthSetup {
            client_id: Some(" abc.apps.googleusercontent.com ".into()),
            client_secret: Some("s3cret".into()),
            scope: Some("https://www.googleapis.com/auth/gmail.readonly".into()),
            callback_port: Some(33418),
        };
        let google = entry("https://gmailmcp.googleapis.com/mcp/v1", &setup, None).unwrap();
        assert_eq!(
            google["oauth"],
            json!({"clientId": "abc.apps.googleusercontent.com", "clientSecret": "s3cret",
                   "scope": "https://www.googleapis.com/auth/gmail.readonly", "callbackPort": 33418})
        );
    }

    #[test]
    fn an_entry_refuses_plain_http_and_a_secret_without_its_client() {
        assert!(entry("http://example.com/mcp", &OAuthSetup::default(), None).is_err());
        assert!(entry("not a url", &OAuthSetup::default(), None).is_err());
        let orphan = OAuthSetup { client_secret: Some("s".into()), ..Default::default() };
        assert!(entry("https://x.example/mcp", &orphan, None).is_err());
    }

    #[test]
    fn reusing_takes_the_client_and_keeps_this_apps_scope() {
        let mut reused = Map::new();
        reused.insert("clientId".into(), "id".into());
        reused.insert("clientSecret".into(), "sec".into());
        reused.insert("scope".into(), "gmail".into());
        let setup = OAuthSetup { client_id: Some("ignored".into()), scope: Some("calendar".into()), ..Default::default() };
        let e = entry("https://calendarmcp.googleapis.com/mcp/v1", &setup, Some(&reused)).unwrap();
        assert_eq!(e["oauth"], json!({"clientId": "id", "clientSecret": "sec", "scope": "calendar"}));
        assert!(entry("https://x.example/mcp", &setup, Some(&Map::new())).is_err());
    }

    #[tokio::test]
    async fn statuses_tell_signing_in_from_keys_and_local_servers() {
        let tmp = tempfile::tempdir().unwrap();
        let (conf, home) = (tmp.path().join("conf"), tmp.path().join("home"));
        std::fs::create_dir_all(&conf).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let servers = json!({"mcpServers": {
            "notion": {"type": "http", "url": "https://mcp.notion.com/mcp"},
            "gmail": {"type": "http", "url": "https://gmailmcp.googleapis.com/mcp/v1", "oauth": {"clientId": "c", "clientSecret": "s"}},
            "github": {"type": "http", "url": "https://api.githubcopilot.com/mcp/", "headers": {"Authorization": "Bearer x"}},
            "kicad": {"command": "uvx", "args": ["mcp-server-kicad"]}
        }});
        std::fs::write(conf.join("mcp.json"), servers.to_string()).unwrap();
        tokens_store(&conf)
            .update(&login_key(ServerSource::Personal, "https://mcp.notion.com/mcp", ""), |_| {
                let mut login = marv_mcp::oauth_store::Login::new("notion");
                login.tokens = Some(serde_json::from_value(json!({"access_token": "a", "token_type": "Bearer"})).unwrap());
                Ok(Some(login))
            })
            .await
            .unwrap();
        let mut got = statuses(&conf, &home);
        got.sort_by(|a, b| a.name.cmp(&b.name));
        let summary: Vec<_> = got.iter().map(|s| (s.name.as_str(), s.kind, s.signed_in, s.has_client)).collect();
        assert_eq!(
            summary,
            vec![
                ("github", "key", false, false),
                ("gmail", "oauth", false, true),
                ("kicad", "local", false, false),
                ("notion", "oauth", true, false),
            ]
        );
        assert_eq!(oauth_of(&conf, "gmail").unwrap()["clientSecret"], "s");
        assert!(oauth_of(&conf, "notion").is_err());
    }
}
