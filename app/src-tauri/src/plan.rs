//! The paid Marv plan (Founding Member, Pro + models) in Settings › Marv plan. The contract is shared with the site's
//! worker and the `marv` CLI (`marv login`): the server's API under `MARV_PLAN_URL` (else [`DEFAULT_URL`]), the signed
//! licence, and `~/.marv/plan.json` (0600, written atomically), which both clients read and write.
//!
//! - Every call to the server goes from here (reqwest), never from the web view: the CSP stays shut. Nothing calls it
//!   unless the person signs in (Free stays account-free).
//! - The server's words are untrusted: `{error}` reaches the page as printable plain text, clipped, never HTML.
//! - The plan's OpenRouter key lives where an OpenRouter key already does (`~/.marv/config.json`'s `apiKey`), so every
//!   path that reads one (`Desktop::api_key`, the chats' `ChatSpec`) uses it. It never overwrites a key the person set
//!   (pasted here, `OPENROUTER_API_KEY`, or their own `apiKey`): the status says so and the page offers "Use the Marv
//!   plan instead". Signing out removes `apiKey` only while it is still the plan's.
//! - The licence is checked offline with the public key compiled in. An invalid or expired licence locks nothing: the
//!   plan shows as Free.

use std::path::{Path, PathBuf};

use base64::Engine;
use marv_provider::ProviderKind;
use marv_session::config::{ConfigStore, FileConfig};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// The server, unless `MARV_PLAN_URL` names another (tests, a staging site).
pub const DEFAULT_URL: &str = "https://www.flologixai.com/api/marv-plan";

/// Who signs in, for the server's sign-up record (`signup_client`).
const CLIENT: &str = "marv-os";

/// The licence signer's Ed25519 public key (32 raw bytes, base64): the production pair, made 2026-11-16 (its private
/// half is only on the server, `MARV_LICENCE_SIGNING_KEY`). The CLI (`marv login`) holds the same key.
pub const MARV_LICENCE_PUBLIC_KEY: &str = "vl8WR97mT5ja+xjzMuZeU3iuBfgKVLY5bovUzOJoBeA=";

/// How long a call may take before the page hears it couldn't reach the server.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// The most of the server's own words shown.
const MAX_SERVER_TEXT: usize = 300;

const OFFLINE: &str = "Couldn't reach the Marv plan server. Check your connection and try again.";
const SIGNED_OUT_THERE: &str = "This computer was signed out of the Marv plan. Sign in again.";

/// The server's base URL from `MARV_PLAN_URL`'s value, if set.
pub fn base_url(env: Option<String>) -> String {
    env.map(|u| u.trim().trim_end_matches('/').to_string()).filter(|u| !u.is_empty()).unwrap_or_else(|| DEFAULT_URL.into())
}

/// This device as the server lists it: "Marv OS on <hostname>" (at most 80 characters).
pub fn device_name(host: &str) -> String {
    let host = marv_core::printable(host).split_whitespace().collect::<Vec<_>>().join(" ");
    let host = if host.is_empty() { "this computer".to_string() } else { host };
    format!("Marv OS on {host}").chars().take(80).collect()
}

/// The machine's name, for [`device_name`].
pub fn hostname() -> String {
    #[cfg(unix)]
    return nix::unistd::gethostname().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default();
    #[cfg(windows)]
    std::env::var("COMPUTERNAME").unwrap_or_default()
}

fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_string(),
    }
}

/// The server's words as the page may show them: printable, on one line, clipped.
fn server_text(text: &str) -> String {
    clip(&marv_core::printable(text).split_whitespace().collect::<Vec<_>>().join(" "), MAX_SERVER_TEXT)
}

pub fn check_email(email: &str) -> Result<String, String> {
    let email = email.trim();
    let ok = email.len() <= 254
        && !email.chars().any(|c| c.is_whitespace() || c.is_control())
        && email.split_once('@').is_some_and(|(user, domain)| !user.is_empty() && domain.contains('.'));
    if ok { Ok(email.to_string()) } else { Err("Enter the email address your Marv plan is under.".into()) }
}

pub fn check_code(code: &str) -> Result<String, String> {
    let code: String = code.chars().filter(|c| !c.is_whitespace() && *c != '-').collect();
    if code.len() == 6 && code.chars().all(|c| c.is_ascii_digit()) {
        Ok(code)
    } else {
        Err("Enter the 6-digit code from the email.".into())
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The licence

/// A licence's payload (contract v1).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Licence {
    pub v: u32,
    pub id: String,
    pub email: String,
    pub plan: String,
    pub issued: u64,
    pub expires: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LicenceCheck {
    /// A free account: signed in, no licence.
    None,
    Valid(Licence),
    Expired(Licence),
    /// Not signed by the key, or not a v1 licence of a known plan.
    Invalid,
}

impl LicenceCheck {
    /// The plan it gives: "founding", "pro_models", or "free" (anything not valid: nothing locks).
    pub fn plan(&self) -> &str {
        match self {
            LicenceCheck::Valid(l) => &l.plan,
            _ => "free",
        }
    }

    fn word(&self) -> &'static str {
        match self {
            LicenceCheck::None => "none",
            LicenceCheck::Valid(_) => "valid",
            LicenceCheck::Expired(_) => "expired",
            LicenceCheck::Invalid => "invalid",
        }
    }
}

pub fn public_key() -> Vec<u8> {
    base64::engine::general_purpose::STANDARD.decode(MARV_LICENCE_PUBLIC_KEY).unwrap_or_default()
}

/// `<base64url(payload)>.<base64url(ed25519 signature over the base64url payload bytes)>`, checked with `key` at `now`
/// (unix seconds).
pub fn check_licence(text: &str, now: u64, key: &[u8]) -> LicenceCheck {
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    if text.trim().is_empty() {
        return LicenceCheck::None;
    }
    let Some((payload, signature)) = text.trim().split_once('.') else { return LicenceCheck::Invalid };
    let Ok(signature) = b64.decode(signature.trim_end_matches('=')) else { return LicenceCheck::Invalid };
    let key = ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key);
    if key.verify(payload.as_bytes(), &signature).is_err() {
        return LicenceCheck::Invalid;
    }
    let Ok(bytes) = b64.decode(payload.trim_end_matches('=')) else { return LicenceCheck::Invalid };
    let Ok(licence) = serde_json::from_slice::<Licence>(&bytes) else { return LicenceCheck::Invalid };
    if licence.v != 1 || !matches!(licence.plan.as_str(), "founding" | "pro_models") {
        return LicenceCheck::Invalid;
    }
    if now >= licence.expires { LicenceCheck::Expired(licence) } else { LicenceCheck::Valid(licence) }
}

// ---------------------------------------------------------------------------------------------------------------
// ~/.marv/plan.json

/// `~/.marv/plan.json`, as the CLI writes it too. Fields this build doesn't know are kept.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanFile {
    pub email: String,
    pub token: String,
    pub plan: String,
    /// Empty for a free account.
    #[serde(default)]
    pub licence: String,
    /// The first 12 characters of the plan's OpenRouter key and "…" (none: the plan has no models).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_label: Option<String>,
    /// The models the plan's key may call, as the server last said: the model menu (here and in the CLI's `/model`)
    /// narrows to them while that key is in use, online or not.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_models: Vec<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

pub fn plan_path(dir: &Path) -> PathBuf {
    dir.join("plan.json")
}

/// The file, if there is one that reads (a broken one counts as signed out; it is replaced at the next sign-in).
pub fn read_plan(dir: &Path) -> Option<PlanFile> {
    serde_json::from_str(&std::fs::read_to_string(plan_path(dir)).ok()?).ok()
}

/// Written to a temp file (0600 from its creation) and renamed over: never half a file, never readable by others.
pub fn write_plan(dir: &Path, plan: &PlanFile) -> Result<(), String> {
    use marv_fs::mode::CreateMode;
    use std::io::Write;
    let fail = |e: std::io::Error| format!("Couldn't save your Marv plan sign-in: {e}");
    std::fs::create_dir_all(dir).map_err(fail)?;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let temp = dir.join(format!(".plan.json.{}.{nanos}.tmp", std::process::id()));
    let text = serde_json::to_string_pretty(plan).map_err(|e| e.to_string())?;
    let written = (|| {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).create_mode(0o600).open(&temp)?;
        file.write_all(text.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        std::fs::rename(&temp, plan_path(dir))?;
        marv_fs::mode::set_mode(&plan_path(dir), 0o600)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written.map_err(fail)
}

pub fn remove_plan(dir: &Path) -> Result<(), String> {
    match std::fs::remove_file(plan_path(dir)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("Couldn't remove {}: {e}", plan_path(dir).display())),
        _ => Ok(()),
    }
}

/// `keyLabel`: the key's first 12 characters and "…". For display only: `sk-or-v1-` is 9 of the 12, so a label can't
/// tell the plan's key from the person's own. A key is the plan's only when `GET /me` returned it exactly.
pub fn key_label(key: &str) -> String {
    format!("{}…", key.chars().take(12).collect::<String>())
}

/// Sign-out left a key it couldn't confirm as the plan's.
pub const KEY_LEFT: &str =
    "The plan's key was left in place because this computer couldn't confirm it; remove it in Settings if you don't use it.";

// ---------------------------------------------------------------------------------------------------------------
// The OpenRouter key, where the person's own lives

/// The OpenRouter keys that come before `~/.marv/config.json`'s on this computer (`Desktop::api_key`'s order).
#[derive(Debug, Clone, Default)]
pub struct Keys {
    /// Pasted in Settings here (the desktop's own settings file).
    pub pasted: Option<String>,
    /// `OPENROUTER_API_KEY`.
    pub env: Option<String>,
}

fn some_key(key: Option<&str>) -> Option<&str> {
    key.map(str::trim).filter(|k| !k.is_empty())
}

fn load_config(dir: &Path) -> Result<Option<FileConfig>, String> {
    ConfigStore::new(dir)
        .load()
        .map_err(|e| format!("Your Marv settings file can't be read ({e}), so the plan's key wasn't saved."))
}

fn save_config(dir: &Path, next: &FileConfig, base: Option<&FileConfig>) -> Result<(), String> {
    ConfigStore::new(dir).save_merged(next, base).map(|_| ()).map_err(|e| format!("Couldn't save your Marv settings: {e}"))
}

/// The OpenRouter key chats here use: pasted, else the environment's, else the config file's.
pub fn effective_key(dir: &Path, keys: &Keys) -> Option<String> {
    if let Some(k) = some_key(keys.pasted.as_deref()).or(some_key(keys.env.as_deref())) {
        return Some(k.to_string());
    }
    load_config(dir).ok().flatten().and_then(|c| some_key(c.api_key.as_deref()).map(str::to_string))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySetup {
    /// The plan's key is now the config file's `apiKey`.
    Installed,
    /// It already was.
    AlreadyIn,
    /// The person has an OpenRouter key of their own here: nothing was changed.
    OwnKey,
}

/// Signing in: the plan's key becomes `apiKey` only when the person has no OpenRouter key of their own; then also
/// provider openrouter and `model` = the plan's default model, only when they have no model of their own (no `model`,
/// and no other provider chosen). Any other key in `apiKey` (an earlier sign-in's included) counts as theirs: only the
/// exact key `GET /me` returned is the plan's.
pub fn install_key(dir: &Path, keys: &Keys, api_key: &str, default_model: Option<&str>) -> Result<KeySetup, String> {
    let theirs = |k: Option<&str>| some_key(k).is_some_and(|k| k != api_key);
    if theirs(keys.pasted.as_deref()) || theirs(keys.env.as_deref()) {
        return Ok(KeySetup::OwnKey);
    }
    let base = load_config(dir)?;
    let file = base.clone().unwrap_or_default();
    match some_key(file.api_key.as_deref()) {
        Some(k) if k == api_key => return Ok(KeySetup::AlreadyIn),
        Some(_) => return Ok(KeySetup::OwnKey),
        None => {}
    }
    let mut next = file.clone();
    next.api_key = Some(api_key.to_string());
    if file.model.is_none()
        && matches!(file.provider, None | Some(ProviderKind::OpenRouter))
        && let Some(model) = default_model.map(str::trim).filter(|m| !m.is_empty())
    {
        next.provider = Some(ProviderKind::OpenRouter);
        next.model = Some(model.to_string());
    }
    save_config(dir, &next, base.as_ref())?;
    Ok(KeySetup::Installed)
}

/// "Use the Marv plan instead": the person chose the plan's key over their own config `apiKey`.
pub fn put_plan_key(dir: &Path, api_key: &str) -> Result<(), String> {
    let base = load_config(dir)?;
    let mut next = base.clone().unwrap_or_default();
    next.api_key = Some(api_key.to_string());
    save_config(dir, &next, base.as_ref())
}

/// Signing out: `apiKey` goes only while it still equals `known`, the plan's key exactly as `GET /me` returned it.
/// Whether it went.
pub fn remove_key(dir: &Path, known: &str) -> Result<bool, String> {
    let Some(base) = load_config(dir)? else { return Ok(false) };
    if some_key(base.api_key.as_deref()) != Some(known) {
        return Ok(false);
    }
    let mut next = base.clone();
    next.api_key = None;
    save_config(dir, &next, Some(&base))?;
    Ok(true)
}

// ---------------------------------------------------------------------------------------------------------------
// The server

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// 401: a bad or revoked token.
    Unauthorized(String),
    /// The server couldn't be reached.
    Offline,
    /// Any other refusal, in the server's words (printable) or ours.
    Server(String),
}

impl ApiError {
    pub fn message(&self) -> String {
        match self {
            ApiError::Unauthorized(m) | ApiError::Server(m) => m.clone(),
            ApiError::Offline => OFFLINE.into(),
        }
    }
}

/// `GET /me`'s answer.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Me {
    pub email: String,
    pub plan: String,
    pub status: String,
    #[serde(default)]
    pub period_end: Option<u64>,
    /// What this subscription costs; absent from a server older than the field.
    #[serde(default)]
    pub price: Option<Price>,
    /// None for a free account (`plan: "free"`).
    #[serde(default)]
    pub licence: Option<String>,
    #[serde(default)]
    pub models: Option<Models>,
    /// Agreed to product emails; absent from a server older than the field.
    #[serde(default)]
    pub marketing: Option<bool>,
}

/// A plan's price as `/me` gives it: whole currency units, an ISO currency code, "month" or "year".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Price {
    pub amount: f64,
    pub currency: String,
    pub interval: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Models {
    #[serde(default)]
    pub provider: Option<String>,
    pub api_key: String,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub limit_usd: f64,
    #[serde(default)]
    pub used_usd: f64,
    #[serde(default)]
    pub resets_at: Option<String>,
    /// The models the plan's key may call (the server's guardrail refuses others): the model menu offers only these
    /// while the plan's key is in use. Empty from an older server: nothing is narrowed.
    #[serde(default)]
    pub allowed_models: Vec<String>,
}

pub struct Client {
    base: String,
    http: reqwest::Client,
}

impl Client {
    pub fn new(base: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        Self { base: base.trim_end_matches('/').to_string(), http }
    }

    async fn call(&self, post: bool, path: &str, token: Option<&str>, body: Option<Value>) -> Result<Value, ApiError> {
        let url = format!("{}{path}", self.base);
        let mut request = if post { self.http.post(&url) } else { self.http.get(&url) };
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request.json(&body);
        } else if post {
            request = request.json(&json!({}));
        }
        let response = request.send().await.map_err(|_| ApiError::Offline)?;
        let status = response.status();
        let text = response.text().await.map_err(|_| ApiError::Offline)?;
        let value: Option<Value> = serde_json::from_str(&text).ok();
        if status.is_success() {
            return value.ok_or_else(|| ApiError::Server("The Marv plan server's answer couldn't be read.".into()));
        }
        // Only `{error}` is shown, as text: an HTML error page from a proxy says nothing and never reaches the page.
        let said = value
            .as_ref()
            .and_then(|v| v.get("error"))
            .and_then(Value::as_str)
            .map(server_text)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("The Marv plan server answered {}.", status.as_u16()));
        Err(if status.as_u16() == 401 { ApiError::Unauthorized(said) } else { ApiError::Server(said) })
    }

    pub async fn login_start(&self, email: &str) -> Result<(), ApiError> {
        self.call(true, "/login/start", None, Some(json!({ "email": email }))).await.map(|_| ())
    }

    /// `marketing`: the person ticked "Email me about Marv updates" (consent; never sent as true otherwise).
    pub async fn login_verify(&self, email: &str, code: &str, device: &str, marketing: bool) -> Result<String, ApiError> {
        let body = json!({ "email": email, "code": code, "device": device, "client": CLIENT, "marketing": marketing });
        let answer = self.call(true, "/login/verify", None, Some(body)).await?;
        let token = answer.get("token").and_then(Value::as_str).unwrap_or_default();
        if token.len() == 64 && token.chars().all(|c| c.is_ascii_hexdigit()) {
            Ok(token.to_string())
        } else {
            Err(ApiError::Server("The Marv plan server's answer couldn't be read.".into()))
        }
    }

    /// Google/GitHub sign-in's last step: the grant from the loopback port and the PKCE verifier, for a device token.
    pub async fn oauth_finish(&self, grant: &str, verifier: &str, device: &str) -> Result<String, ApiError> {
        let body = json!({ "grant": grant, "verifier": verifier, "device": device });
        let answer = self.call(true, "/oauth/finish", None, Some(body)).await?;
        let token = answer.get("token").and_then(Value::as_str).unwrap_or_default();
        if token.len() == 64 && token.chars().all(|c| c.is_ascii_hexdigit()) {
            Ok(token.to_string())
        } else {
            Err(ApiError::Server("The Marv plan server's answer couldn't be read.".into()))
        }
    }

    /// The person's own switch for product emails.
    pub async fn set_marketing(&self, token: &str, on: bool) -> Result<(), ApiError> {
        self.call(true, "/marketing", Some(token), Some(json!({ "marketing": on }))).await.map(|_| ())
    }

    pub async fn me(&self, token: &str) -> Result<Me, ApiError> {
        let answer = self.call(false, "/me", Some(token), None).await?;
        serde_json::from_value(answer).map_err(|_| ApiError::Server("The Marv plan server's answer couldn't be read.".into()))
    }

    pub async fn logout(&self, token: &str) -> Result<(), ApiError> {
        self.call(true, "/logout", Some(token), None).await.map(|_| ())
    }

    /// The billing portal's address (https only).
    pub async fn portal(&self, token: &str) -> Result<String, ApiError> {
        let answer = self.call(true, "/portal", Some(token), None).await?;
        let url = answer.get("url").and_then(Value::as_str).unwrap_or_default();
        if url.starts_with("https://") && !url.chars().any(|c| c.is_whitespace() || c.is_control()) {
            Ok(url.to_string())
        } else {
            Err(ApiError::Server("The Marv plan server's answer couldn't be read.".into()))
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// What the page sees

/// The monthly model allowance (Pro + models). The key itself never reaches the page.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Allowance {
    pub limit_usd: f64,
    pub used_usd: f64,
    pub resets_at: Option<String>,
    pub default_model: Option<String>,
    pub allowed_models: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStatus {
    pub signed_in: bool,
    pub email: Option<String>,
    /// "founding" or "pro_models", as the server (or, offline, plan.json) says.
    pub plan: Option<String>,
    /// "active", "past_due" or "canceled"; None offline.
    pub status: Option<String>,
    pub period_end: Option<u64>,
    /// Online: what the subscription costs (server text, made printable). None offline.
    pub price: Option<Price>,
    /// "valid", "expired", "invalid" or "none".
    pub licence: &'static str,
    pub licence_expires: Option<u64>,
    /// What this computer counts the plan as, from the licence alone: "founding", "pro_models" or "free".
    pub effective: String,
    /// Online, Pro + models: the allowance.
    pub allowance: Option<Allowance>,
    /// The plan includes models (a key).
    /// The models the model menu offers from OpenRouter while the plan's key is in use (empty: not narrowed).
    pub menu_models: Vec<String>,
    pub has_models: bool,
    /// The plan's key is the OpenRouter key chats here use.
    pub key_in_use: bool,
    /// Another OpenRouter key of the person's is in use instead.
    pub own_key: bool,
    /// The server answered this time.
    pub online: bool,
    pub notice: Option<String>,
    /// Online: whether the person agreed to product emails.
    pub marketing: Option<bool>,
}

impl PlanStatus {
    pub fn signed_out() -> Self {
        Self {
            signed_in: false,
            email: None,
            plan: None,
            status: None,
            period_end: None,
            price: None,
            licence: "none",
            licence_expires: None,
            effective: "free".into(),
            allowance: None,
            menu_models: Vec::new(),
            has_models: false,
            key_in_use: false,
            own_key: false,
            online: false,
            notice: None,
            marketing: None,
        }
    }
}

/// The status from what's known: plan.json, the server's answer if it came, and the key in use here.
pub fn status_from(file: &PlanFile, me: Option<&Me>, now: u64, key: &[u8], effective_key: Option<&str>) -> PlanStatus {
    let licence = check_licence(me.map_or(file.licence.as_str(), |m| m.licence.as_deref().unwrap_or_default()), now, key);
    let licence_expires = match &licence {
        LicenceCheck::Valid(l) | LicenceCheck::Expired(l) => Some(l.expires),
        LicenceCheck::None | LicenceCheck::Invalid => None,
    };
    let models = me.and_then(|m| m.models.as_ref());
    let has_models = match me {
        Some(m) => m.models.is_some(),
        None => file.key_label.is_some(),
    };
    // Only the server's exact key says which key is the plan's; offline, neither is claimed.
    let key_in_use = matches!((effective_key, models), (Some(k), Some(m)) if k == m.api_key);
    PlanStatus {
        signed_in: true,
        email: Some(server_text(me.map_or(&file.email, |m| &m.email))),
        plan: Some(server_text(me.map_or(&file.plan, |m| &m.plan))),
        status: me.map(|m| server_text(&m.status)),
        period_end: me.and_then(|m| m.period_end),
        price: me.and_then(|m| m.price.as_ref()).map(|p| Price {
            amount: p.amount,
            currency: server_text(&p.currency),
            interval: server_text(&p.interval),
        }),
        licence: licence.word(),
        licence_expires,
        effective: licence.plan().to_string(),
        allowance: models.map(|m| Allowance {
            limit_usd: m.limit_usd,
            used_usd: m.used_usd,
            resets_at: m.resets_at.as_deref().map(server_text),
            default_model: m.default_model.as_deref().map(server_text),
            allowed_models: m.allowed_models.iter().map(|id| server_text(id)).collect(),
        }),
        // The menu narrows while the plan's key is in use: the server's exact key online; offline, the key here has the
        // label plan.json kept for the plan's key.
        menu_models: if key_in_use {
            models.map(|m| m.allowed_models.clone()).unwrap_or_default()
        } else if me.is_none() && effective_key.is_some_and(|k| file.key_label.as_deref() == Some(key_label(k).as_str())) {
            file.allowed_models.clone()
        } else {
            Vec::new()
        },
        has_models,
        key_in_use,
        own_key: models.is_some() && effective_key.is_some() && !key_in_use,
        online: me.is_some(),
        notice: None,
        marketing: me.and_then(|m| m.marketing),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The commands' work (lib.rs wraps these with the desktop's own settings)

/// Where the commands work: Marv's folder, the server, the keys before the config file's, and the time.
pub struct Ctx {
    pub dir: PathBuf,
    pub base: String,
    pub keys: Keys,
    pub now: u64,
    pub public_key: Vec<u8>,
}

impl Ctx {
    fn client(&self) -> Client {
        Client::new(&self.base)
    }

    fn effective_key(&self) -> Option<String> {
        effective_key(&self.dir, &self.keys)
    }

    fn token(&self) -> Result<PlanFile, String> {
        read_plan(&self.dir).ok_or_else(|| "Sign in to the Marv plan first.".to_string())
    }
}

pub async fn login_start(ctx: &Ctx, email: &str) -> Result<(), String> {
    let email = check_email(email)?;
    ctx.client().login_start(&email).await.map_err(|e| e.message())
}

/// Signed in: plan.json written, the key set up per [`install_key`], and the status.
pub async fn login_verify(
    ctx: &Ctx,
    email: &str,
    code: &str,
    device: &str,
    marketing: bool,
) -> Result<(PlanStatus, Option<KeySetup>), String> {
    let (email, code) = (check_email(email)?, check_code(code)?);
    let client = ctx.client();
    let token = client.login_verify(&email, &code, device, marketing).await.map_err(|e| e.message())?;
    signed_in(ctx, &client, token).await
}

/// A new device token: its account loaded, plan.json written, the plan's key set up.
async fn signed_in(ctx: &Ctx, client: &Client, token: String) -> Result<(PlanStatus, Option<KeySetup>), String> {
    let me = match client.me(&token).await {
        Ok(me) => me,
        Err(e) => {
            let _ = client.logout(&token).await;
            return Err(format!("Signed in, but your account couldn't be loaded: {} Try again.", e.message()));
        }
    };
    let file = PlanFile {
        email: me.email.clone(),
        token,
        plan: me.plan.clone(),
        licence: me.licence.clone().unwrap_or_default(),
        key_label: me.models.as_ref().map(|m| key_label(&m.api_key)),
        allowed_models: me.models.as_ref().map(|m| m.allowed_models.clone()).unwrap_or_default(),
        extra: Map::new(),
    };
    write_plan(&ctx.dir, &file)?;
    let mut notice = None;
    let installed = match &me.models {
        Some(models) => match install_key(&ctx.dir, &ctx.keys, &models.api_key, models.default_model.as_deref()) {
            Ok(installed) => Some(installed),
            Err(e) => {
                notice = Some(e);
                None
            }
        },
        None => None,
    };
    let mut status = status_from(&file, Some(&me), ctx.now, &ctx.public_key, ctx.effective_key().as_deref());
    status.notice = notice;
    Ok((status, installed))
}

/// How long a Google/GitHub sign-in waits for the browser to come back.
const BROWSER_WAIT: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// The Google/GitHub sign-in in progress here, if any: a new one (or Cancel) ends it.
static SIGNING_IN: std::sync::Mutex<Option<tokio_util::sync::CancellationToken>> = std::sync::Mutex::new(None);

/// Ends the Google/GitHub sign-in in progress, if any.
pub fn cancel_sign_in() {
    if let Some(cancel) = SIGNING_IN.lock().unwrap_or_else(|e| e.into_inner()).take() {
        cancel.cancel();
    }
}

/// The loopback address that starts a sign-in with `provider` at `base`.
pub fn sign_in_url(base: &str, provider: &str, challenge: &str, state: &str, port: u16, marketing: bool) -> String {
    let consent = if marketing { "&marketing=1" } else { "" };
    format!(
        "{}/oauth/{provider}/start?challenge={challenge}&state={state}&port={port}&client={CLIENT}{consent}",
        base.trim_end_matches('/')
    )
}

/// Why the server sent the browser back without a grant, as the person reads it.
fn browser_refusal(failure: &str) -> String {
    if failure.contains("(no_verified_email)") {
        "That account has no verified email address. Verify one there, or sign in with your email.".into()
    } else if failure.contains("(cancelled)") || failure.contains("(access_denied)") {
        "Sign-in was cancelled in the browser.".into()
    } else {
        "Sign-in didn't finish in the browser. Try again, or sign in with your email.".into()
    }
}

/// Signs in with Google or GitHub (RFC 8252): a listener on 127.0.0.1, the browser opened at the server's start page
/// with a PKCE challenge, and the one-time grant it sends back traded with the verifier for a device token.
pub async fn oauth_login(
    ctx: &Ctx,
    provider: &str,
    device: &str,
    marketing: bool,
    open: impl Fn(&str),
) -> Result<(PlanStatus, Option<KeySetup>), String> {
    use marv_mcp::oauth::{CallbackExpectation, listen, new_state, pkce};
    if !matches!(provider, "google" | "github") {
        return Err("Sign in with Google, GitHub or your email.".into());
    }
    let cancel = tokio_util::sync::CancellationToken::new();
    if let Some(previous) = SIGNING_IN.lock().unwrap_or_else(|e| e.into_inner()).replace(cancel.clone()) {
        previous.cancel();
    }
    let (verifier, challenge) = pkce();
    let state = new_state();
    let expectation = std::sync::Arc::new(std::sync::Mutex::new(CallbackExpectation::new(state.clone())));
    let mut outcome = expectation.lock().unwrap_or_else(|e| e.into_inner()).outcome();
    let check = expectation.clone();
    let listener =
        listen(std::sync::Arc::new(move |url: &url::Url| check.lock().unwrap_or_else(|e| e.into_inner()).check(url)), None)
            .await
            .map_err(|e| format!("Couldn't start the sign-in: {e}"))?;
    open(&sign_in_url(&ctx.base, provider, &challenge, &state, listener.port, marketing));
    let came = tokio::select! {
        _ = cancel.cancelled() => return Err("Sign-in was cancelled.".into()),
        _ = tokio::time::sleep(BROWSER_WAIT) => return Err("The browser didn't come back. Try again.".into()),
        came = outcome.wait_for(Option::is_some) => came.map(|c| c.clone()),
    };
    listener.close();
    let grant = match came {
        Ok(Some(Ok(grant))) => grant,
        Ok(Some(Err(failure))) => return Err(browser_refusal(&failure)),
        _ => return Err("Sign-in was cancelled.".into()),
    };
    let client = ctx.client();
    let token = client.oauth_finish(&grant, &verifier, device).await.map_err(|e| e.message())?;
    signed_in(ctx, &client, token).await
}

/// The status: the server's when it answers (`online`), else what plan.json and the licence say.
pub async fn status(ctx: &Ctx, online: bool) -> PlanStatus {
    let Some(file) = read_plan(&ctx.dir) else { return PlanStatus::signed_out() };
    let answer = if online { Some(ctx.client().me(&file.token).await) } else { None };
    let key = ctx.effective_key();
    match answer {
        Some(Ok(me)) => {
            let next = PlanFile {
                plan: me.plan.clone(),
                licence: me.licence.clone().unwrap_or_default(),
                key_label: me.models.as_ref().map(|m| key_label(&m.api_key)).or(file.key_label.clone()),
                allowed_models: me.models.as_ref().map_or_else(|| file.allowed_models.clone(), |m| m.allowed_models.clone()),
                ..file.clone()
            };
            if next != file {
                let _ = write_plan(&ctx.dir, &next);
            }
            status_from(&next, Some(&me), ctx.now, &ctx.public_key, key.as_deref())
        }
        Some(Err(e)) => {
            let mut status = status_from(&file, None, ctx.now, &ctx.public_key, key.as_deref());
            status.notice = Some(match e {
                ApiError::Unauthorized(_) => SIGNED_OUT_THERE.into(),
                ApiError::Offline => "Offline: showing what this computer knows.".into(),
                ApiError::Server(m) => m,
            });
            status
        }
        None => status_from(&file, None, ctx.now, &ctx.public_key, key.as_deref()),
    }
}

/// What signing out did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedOut {
    /// The plan's key left the config file.
    pub removed: bool,
    /// [`KEY_LEFT`], when a key stayed that this computer couldn't confirm wasn't the plan's.
    pub notice: Option<String>,
}

/// Signs out: the plan's key out of the config file only when `GET /me` returned it exactly and it still equals the
/// stored one (never by its label), the token revoked when the server answers, plan.json deleted.
pub async fn logout(ctx: &Ctx) -> Result<SignedOut, String> {
    let Some(file) = read_plan(&ctx.dir) else { return Ok(SignedOut { removed: false, notice: None }) };
    let client = ctx.client();
    let known = client.me(&file.token).await.ok().and_then(|m| m.models).map(|m| m.api_key);
    let removed = match &known {
        Some(known) => remove_key(&ctx.dir, known).unwrap_or(false),
        None => false,
    };
    // The plan had a key, the server couldn't say which, and a key is there: kept, and the person told.
    let unconfirmed = known.is_none()
        && file.key_label.is_some()
        && load_config(&ctx.dir).ok().flatten().is_some_and(|c| some_key(c.api_key.as_deref()).is_some());
    let _ = client.logout(&file.token).await;
    remove_plan(&ctx.dir)?;
    Ok(SignedOut { removed, notice: unconfirmed.then(|| KEY_LEFT.to_string()) })
}

/// Product emails on or off, then the status (the server's).
pub async fn set_marketing(ctx: &Ctx, on: bool) -> Result<PlanStatus, String> {
    let file = ctx.token()?;
    ctx.client().set_marketing(&file.token, on).await.map_err(|e| match e {
        ApiError::Unauthorized(_) => SIGNED_OUT_THERE.into(),
        e => e.message(),
    })?;
    Ok(status(ctx, true).await)
}

/// The billing portal's address.
pub async fn portal(ctx: &Ctx) -> Result<String, String> {
    let file = ctx.token()?;
    ctx.client().portal(&file.token).await.map_err(|e| match e {
        ApiError::Unauthorized(_) => SIGNED_OUT_THERE.into(),
        e => e.message(),
    })
}

/// "Use the Marv plan instead": the plan's key into the config file. The caller clears the key pasted here.
pub async fn use_plan_key(ctx: &Ctx) -> Result<(), String> {
    let file = ctx.token()?;
    let me = ctx.client().me(&file.token).await.map_err(|e| match e {
        ApiError::Unauthorized(_) => SIGNED_OUT_THERE.into(),
        e => e.message(),
    })?;
    let models = me.models.ok_or("Your plan doesn't include models.")?;
    if some_key(ctx.keys.env.as_deref()).is_some_and(|k| k != models.api_key) {
        return Err("OPENROUTER_API_KEY is set where Marv OS started, and it comes first. Unset it to use the plan's key.".into());
    }
    put_plan_key(&ctx.dir, &models.api_key)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// The dev signing key (PKCS#8 v1), published in the site's runbook: for these tests only, never what ships.
    const DEV_SIGNING_KEY: &str = "MC4CAQAwBQYDK2VwBCIEIEm0aG2Nc+lNnN2WE3Bri8UFh/U5NiY8VKGaMRMySHDa";
    const NOW: u64 = 1_790_000_000;
    const PLAN_KEY: &str = "sk-or-v1-0123456789abcdef0123456789abcdef";
    const TOKEN: &str = "ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12";

    /// The dev pair's public half: what these tests' licences are checked with (the shipped key is the production one).
    fn dev_public_key() -> Vec<u8> {
        use ring::signature::KeyPair;
        signer().public_key().as_ref().to_vec()
    }

    fn signer() -> ring::signature::Ed25519KeyPair {
        let pkcs8 = base64::engine::general_purpose::STANDARD.decode(DEV_SIGNING_KEY).unwrap();
        ring::signature::Ed25519KeyPair::from_pkcs8_maybe_unchecked(&pkcs8).unwrap()
    }

    fn licence_with(payload: Value, key: &ring::signature::Ed25519KeyPair) -> String {
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let payload = b64.encode(payload.to_string());
        let signature = b64.encode(key.sign(payload.as_bytes()).as_ref());
        format!("{payload}.{signature}")
    }

    fn licence(plan: &str, expires: u64) -> String {
        licence_with(
            json!({ "v": 1, "id": "acct_1", "email": "a@b.co", "plan": plan, "issued": NOW - 100, "expires": expires }),
            &signer(),
        )
    }

    #[test]
    fn the_compiled_in_key_is_never_the_dev_signers() {
        // The shipped key is never the dev pair's, whose private half is published: anyone could sign with it.
        assert_ne!(dev_public_key(), public_key(), "MARV_LICENCE_PUBLIC_KEY is the dev key");
        assert_eq!(public_key().len(), 32, "an Ed25519 public key");
        assert!(
            check_licence(&licence("founding", NOW + 3600), NOW, &public_key()) == LicenceCheck::Invalid,
            "a dev licence isn't valid"
        );
    }

    #[test]
    fn a_valid_licence_gives_its_plan() {
        let check = check_licence(&licence("pro_models", NOW + 3600), NOW, &dev_public_key());
        assert!(matches!(&check, LicenceCheck::Valid(l) if l.plan == "pro_models" && l.email == "a@b.co"));
        assert_eq!(check.plan(), "pro_models");
        assert_eq!(check_licence(&licence("founding", NOW + 1), NOW, &dev_public_key()).plan(), "founding");
    }

    #[test]
    fn an_expired_licence_is_free() {
        let check = check_licence(&licence("founding", NOW), NOW, &dev_public_key());
        assert!(matches!(check, LicenceCheck::Expired(_)));
        assert_eq!(check.plan(), "free");
    }

    #[test]
    fn a_tampered_licence_is_free() {
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let good = licence("founding", NOW + 3600);
        let (_, signature) = good.split_once('.').unwrap();
        // Another plan, a later expiry: the signature no longer matches.
        let forged = b64.encode(
            json!({ "v": 1, "id": "acct_1", "email": "a@b.co", "plan": "pro_models", "issued": 1, "expires": NOW * 2 })
                .to_string(),
        );
        assert_eq!(check_licence(&format!("{forged}.{signature}"), NOW, &dev_public_key()), LicenceCheck::Invalid);
        // Signed by another key.
        let rng = ring::rand::SystemRandom::new();
        let other =
            ring::signature::Ed25519KeyPair::from_pkcs8(ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap().as_ref())
                .unwrap();
        let theirs = licence_with(
            json!({ "v": 1, "id": "x", "email": "a@b.co", "plan": "founding", "issued": 1, "expires": NOW + 10 }),
            &other,
        );
        assert_eq!(check_licence(&theirs, NOW, &dev_public_key()).plan(), "free");
        // Not a licence, an unknown version or plan.
        for text in ["abc", "a.b.c", "!!!.???"] {
            assert_eq!(check_licence(text, NOW, &dev_public_key()), LicenceCheck::Invalid, "{text}");
        }
        // No licence at all is a free account's: none, still free.
        assert_eq!(check_licence(" ", NOW, &dev_public_key()), LicenceCheck::None);
        assert_eq!(LicenceCheck::None.plan(), "free");
        let v2 = licence_with(
            json!({ "v": 2, "id": "x", "email": "a@b.co", "plan": "founding", "issued": 1, "expires": NOW + 10 }),
            &signer(),
        );
        assert_eq!(check_licence(&v2, NOW, &dev_public_key()), LicenceCheck::Invalid);
        let team = licence_with(
            json!({ "v": 1, "id": "x", "email": "a@b.co", "plan": "team", "issued": 1, "expires": NOW + 10 }),
            &signer(),
        );
        assert_eq!(check_licence(&team, NOW, &dev_public_key()), LicenceCheck::Invalid);
    }

    #[test]
    fn plan_json_is_0600_atomic_and_keeps_fields_it_doesnt_know() {
        let dir = tempfile::tempdir().unwrap();
        let mut extra = Map::new();
        extra.insert("cliOnly".into(), json!(true));
        let file = PlanFile {
            email: "a@b.co".into(),
            token: TOKEN.into(),
            plan: "pro_models".into(),
            licence: "x.y".into(),
            key_label: Some(key_label(PLAN_KEY)),
            allowed_models: vec!["deepseek/deepseek-v4.1-flash".into()],
            extra,
        };
        write_plan(dir.path(), &file).unwrap();
        #[cfg(unix)] // Windows has no mode bits
        assert_eq!(std::fs::metadata(plan_path(dir.path())).unwrap().permissions().mode() & 0o777, 0o600);
        let text = std::fs::read_to_string(plan_path(dir.path())).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["keyLabel"], "sk-or-v1-012…");
        assert_eq!(value["allowedModels"], json!(["deepseek/deepseek-v4.1-flash"]), "the CLI reads the same field");
        assert_eq!(value["cliOnly"], true);
        assert_eq!(read_plan(dir.path()), Some(file));
        let leftovers = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(leftovers, 1, "no temp file left behind");
        remove_plan(dir.path()).unwrap();
        remove_plan(dir.path()).unwrap();
        assert_eq!(read_plan(dir.path()), None);
    }

    fn config(dir: &Path) -> FileConfig {
        ConfigStore::new(dir).load().unwrap().unwrap_or_default()
    }

    fn set_config(dir: &Path, file: &FileConfig) {
        ConfigStore::new(dir).save(file).unwrap();
    }

    #[test]
    fn signing_in_sets_the_key_provider_and_model_only_where_the_person_has_none() {
        let dir = tempfile::tempdir().unwrap();
        let installed = install_key(dir.path(), &Keys::default(), PLAN_KEY, Some("z-ai/glm-5.3")).unwrap();
        assert_eq!(installed, KeySetup::Installed);
        let c = config(dir.path());
        assert_eq!(c.api_key.as_deref(), Some(PLAN_KEY));
        assert_eq!(c.provider, Some(ProviderKind::OpenRouter));
        assert_eq!(c.model.as_deref(), Some("z-ai/glm-5.3"));
        assert_eq!(install_key(dir.path(), &Keys::default(), PLAN_KEY, None).unwrap(), KeySetup::AlreadyIn);

        // A model of their own: the key goes in, their model and provider stay.
        let dir = tempfile::tempdir().unwrap();
        set_config(dir.path(), &FileConfig { model: Some("their/model".into()), ..FileConfig::default() });
        install_key(dir.path(), &Keys::default(), PLAN_KEY, Some("z-ai/glm-5.3")).unwrap();
        let c = config(dir.path());
        assert_eq!((c.api_key.as_deref(), c.model.as_deref(), c.provider), (Some(PLAN_KEY), Some("their/model"), None));

        // Another provider chosen: not switched.
        let dir = tempfile::tempdir().unwrap();
        set_config(dir.path(), &FileConfig { provider: Some(ProviderKind::Kimi), ..FileConfig::default() });
        install_key(dir.path(), &Keys::default(), PLAN_KEY, Some("z-ai/glm-5.3")).unwrap();
        let c = config(dir.path());
        assert_eq!((c.provider, c.model), (Some(ProviderKind::Kimi), None));
    }

    #[test]
    fn signing_in_never_overwrites_a_key_the_person_set() {
        let dir = tempfile::tempdir().unwrap();
        set_config(dir.path(), &FileConfig { api_key: Some("sk-or-v1-mine".into()), ..FileConfig::default() });
        assert_eq!(install_key(dir.path(), &Keys::default(), PLAN_KEY, Some("m/x")).unwrap(), KeySetup::OwnKey);
        assert_eq!(config(dir.path()).api_key.as_deref(), Some("sk-or-v1-mine"));
        assert_eq!(config(dir.path()).model, None);

        // Pasted here, or in the environment: the config file isn't touched.
        for keys in
            [Keys { pasted: Some("sk-or-v1-pasted".into()), env: None }, Keys { pasted: None, env: Some("sk-or-v1-env".into()) }]
        {
            let dir = tempfile::tempdir().unwrap();
            assert_eq!(install_key(dir.path(), &keys, PLAN_KEY, Some("m/x")).unwrap(), KeySetup::OwnKey);
            assert_eq!(ConfigStore::new(dir.path()).load().unwrap(), None);
        }

        // A key that shares the plan key's label (an earlier sign-in's, or theirs): never replaced by its label.
        let dir = tempfile::tempdir().unwrap();
        let alike = "sk-or-v1-012ffffffffffffff";
        assert_eq!(key_label(alike), key_label(PLAN_KEY));
        set_config(dir.path(), &FileConfig { api_key: Some(alike.into()), ..FileConfig::default() });
        assert_eq!(install_key(dir.path(), &Keys::default(), PLAN_KEY, None).unwrap(), KeySetup::OwnKey);
        assert_eq!(config(dir.path()).api_key.as_deref(), Some(alike));
    }

    #[test]
    fn signing_out_removes_only_the_plans_own_key() {
        let dir = tempfile::tempdir().unwrap();
        set_config(dir.path(), &FileConfig { api_key: Some("sk-or-v1-mine".into()), ..FileConfig::default() });
        assert!(!remove_key(dir.path(), PLAN_KEY).unwrap());
        assert_eq!(config(dir.path()).api_key.as_deref(), Some("sk-or-v1-mine"));

        set_config(
            dir.path(),
            &FileConfig { api_key: Some(PLAN_KEY.into()), model: Some("m/x".into()), ..FileConfig::default() },
        );
        assert!(remove_key(dir.path(), PLAN_KEY).unwrap());
        let c = config(dir.path());
        assert_eq!((c.api_key, c.model.as_deref()), (None, Some("m/x")));
        assert!(!remove_key(dir.path(), PLAN_KEY).unwrap());
        assert!(!remove_key(tempfile::tempdir().unwrap().path(), PLAN_KEY).unwrap(), "no config file: nothing to do");
    }

    #[test]
    fn device_names_and_inputs() {
        assert_eq!(device_name("ws-1"), "Marv OS on ws-1");
        assert_eq!(device_name(""), "Marv OS on this computer");
        assert_eq!(device_name(&"x".repeat(200)).chars().count(), 80);
        assert_eq!(device_name("a\u{1b}[31mb\nc"), "Marv OS on ab c");
        assert_eq!(check_code(" 123 456 "), Ok("123456".into()));
        assert!(check_code("12345").is_err() && check_code("abcdef").is_err());
        assert_eq!(check_email(" a@b.co "), Ok("a@b.co".into()));
        assert!(check_email("a@b").is_err() && check_email("a b@c.d").is_err() && check_email("@c.d").is_err());
        assert_eq!(base_url(None), DEFAULT_URL);
        assert_eq!(base_url(Some(" http://127.0.0.1:9/x/ ".into())), "http://127.0.0.1:9/x");
    }

    // -----------------------------------------------------------------------------------------------------------
    // A local stand-in for the server

    #[derive(Debug, Clone)]
    struct Seen {
        method: String,
        path: String,
        auth: Option<String>,
        body: Value,
    }

    type Route = Arc<dyn Fn(&Seen) -> (u16, String) + Send + Sync>;

    /// A tiny HTTP/1.1 server on 127.0.0.1: `route` answers each request; every request is recorded.
    async fn stub(route: Route) -> (String, Arc<Mutex<Vec<Seen>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let (route, log) = (route.clone(), log.clone());
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let head_end = loop {
                        let n = socket.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break at + 4;
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                    let mut lines = head.split("\r\n");
                    let mut first = lines.next().unwrap_or_default().split(' ');
                    let (method, path) =
                        (first.next().unwrap_or_default().to_string(), first.next().unwrap_or_default().to_string());
                    let header = |name: &str| {
                        head.split("\r\n").find_map(|l| {
                            let (n, v) = l.split_once(':')?;
                            n.trim().eq_ignore_ascii_case(name).then(|| v.trim().to_string())
                        })
                    };
                    let length: usize = header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
                    while buf.len() < head_end + length {
                        let n = socket.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    let body = serde_json::from_slice(&buf[head_end..]).unwrap_or(Value::Null);
                    let request = Seen { method, path, auth: header("authorization"), body };
                    let (status, text) = route(&request);
                    log.lock().unwrap().push(request);
                    let reply = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
                        text.len()
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        (base, seen)
    }

    fn me_json(plan: &str, models: bool) -> String {
        let models = if models {
            json!({ "provider": "openrouter", "apiKey": PLAN_KEY, "defaultModel": "z-ai/glm-5.3",
                    "limitUsd": 8, "usedUsd": 1.25, "resetsAt": "2026-12-01T00:00:00Z",
                    "allowedModels": ["z-ai/glm-5.3", "deepseek/deepseek-v4.1-flash"] })
        } else {
            Value::Null
        };
        json!({ "email": "a@b.co", "plan": plan, "status": "active", "periodEnd": NOW + 86_400,
                "licence": licence(plan, NOW + 86_400), "models": models })
        .to_string()
    }

    /// `/me`'s price reaches the page, made printable; a server without the field gives none.
    #[test]
    fn the_plans_price_comes_from_the_server() {
        let file = PlanFile {
            email: "a@b.co".into(),
            token: TOKEN.into(),
            plan: "pro_models".into(),
            licence: licence("pro_models", NOW + 86_400),
            key_label: None,
            allowed_models: Vec::new(),
            extra: Map::new(),
        };
        let mut v: Value = serde_json::from_str(&me_json("pro_models", true)).unwrap();
        let old: Me = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(status_from(&file, Some(&old), NOW, &dev_public_key(), None).price, None);
        v["price"] = json!({ "amount": 20, "currency": "usd\u{1b}[31m", "interval": "month" });
        let me: Me = serde_json::from_value(v).unwrap();
        let price = status_from(&file, Some(&me), NOW, &dev_public_key(), None).price.unwrap();
        assert_eq!((price.amount, price.interval.as_str()), (20.0, "month"));
        assert!(!price.currency.contains('\u{1b}'), "{}", price.currency);
        assert_eq!(status_from(&file, None, NOW, &dev_public_key(), None).price, None, "offline: no price");
    }

    /// The happy server: codes 123456, token [`TOKEN`].
    fn server(plan: &'static str, models: bool) -> Route {
        Arc::new(move |r: &Seen| match (r.method.as_str(), r.path.as_str()) {
            ("POST", "/login/start") => (200, r#"{"ok":true}"#.into()),
            ("POST", "/login/verify") if r.body["code"] == "123456" => (200, json!({ "token": TOKEN }).to_string()),
            ("POST", "/login/verify") => (400, r#"{"error":"That code <b>isn't right</b>.\u001b[31m"}"#.into()),
            (_, _) if r.auth.as_deref() != Some(&format!("Bearer {TOKEN}")) => (401, r#"{"error":"Sign in again."}"#.into()),
            ("GET", "/me") => (200, me_json(plan, models)),
            ("POST", "/logout") => (200, r#"{"ok":true}"#.into()),
            ("POST", "/portal") => (200, r#"{"url":"https://billing.stripe.com/p/session/x"}"#.into()),
            _ => (404, "<html>nope</html>".into()),
        })
    }

    fn ctx(dir: &Path, base: &str) -> Ctx {
        Ctx { dir: dir.to_path_buf(), base: base.into(), keys: Keys::default(), now: NOW, public_key: dev_public_key() }
    }

    /// A free account (anyone who signs in): signed in, no licence, no models, nothing claimed as paid.
    #[tokio::test]
    async fn a_free_account_signs_in_with_no_licence() {
        let free = json!({ "email": "a@b.co", "plan": "free", "status": "active", "periodEnd": null,
                           "price": null, "licence": null, "models": null })
        .to_string();
        let (base, _) = stub(Arc::new(move |r: &Seen| match r.path.as_str() {
            "/login/verify" => (200, json!({ "token": TOKEN }).to_string()),
            "/me" => (200, free.clone()),
            _ => (404, String::new()),
        }))
        .await;
        let dir = tempfile::tempdir().unwrap();
        let (status, installed) =
            login_verify(&ctx(dir.path(), &base), "a@b.co", "123456", "Marv OS on ws", false).await.unwrap();
        assert_eq!(installed, None);
        assert!(status.signed_in && status.online && !status.has_models);
        assert_eq!((status.plan.as_deref(), status.licence, status.effective.as_str()), (Some("free"), "none", "free"));
        assert_eq!(read_plan(dir.path()).unwrap().licence, "");
        // Offline, plan.json alone says the same.
        let offline = status_from(&read_plan(dir.path()).unwrap(), None, NOW, &dev_public_key(), None);
        assert_eq!((offline.licence, offline.effective.as_str()), ("none", "free"));
    }

    /// One browser sign-in at a time in these tests: they share the sign-in in progress.
    static BROWSER: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// The server for a browser sign-in: `/oauth/finish` gives [`TOKEN`] for grant "g".repeat(64), recording the
    /// verifier; `/me` a free account.
    fn browser_server() -> Route {
        Arc::new(|r: &Seen| match (r.method.as_str(), r.path.as_str()) {
            ("POST", "/oauth/finish") if r.body["grant"] == "a".repeat(64) => (200, json!({ "token": TOKEN }).to_string()),
            ("POST", "/oauth/finish") => (400, r#"{"error":"This sign-in has expired. Start again."}"#.into()),
            ("GET", "/me") => {
                (200, json!({ "email": "g@b.co", "plan": "free", "status": "active", "licence": null }).to_string())
            }
            _ => (404, String::new()),
        })
    }

    /// What the browser does after the provider: back to the start URL's port with its state and `query`.
    fn browser(query: &'static str, opened: Arc<Mutex<Option<url::Url>>>) -> impl Fn(&str) {
        move |start: &str| {
            let start = url::Url::parse(start).unwrap();
            let param = |k: &str| start.query_pairs().find(|(q, _)| q == k).map(|(_, v)| v.into_owned()).unwrap();
            let back = format!("http://127.0.0.1:{}/callback?state={}&{query}", param("port"), param("state"));
            *opened.lock().unwrap() = Some(start);
            tokio::spawn(async move {
                let _ = reqwest::get(back).await;
            });
        }
    }

    #[tokio::test]
    async fn google_sign_in_comes_back_to_the_port_and_trades_the_grant_with_the_verifier() {
        let _one = BROWSER.lock().await;
        let (base, seen) = stub(browser_server()).await;
        let dir = tempfile::tempdir().unwrap();
        let opened = Arc::new(Mutex::new(None));
        let grant: &'static str = Box::leak(format!("code={}", "a".repeat(64)).into_boxed_str());
        let (status, _) =
            oauth_login(&ctx(dir.path(), &base), "google", "Marv OS on ws", true, browser(grant, opened.clone())).await.unwrap();
        assert_eq!((status.email.as_deref(), status.plan.as_deref()), (Some("g@b.co"), Some("free")));
        assert_eq!(read_plan(dir.path()).unwrap().token, TOKEN);
        let start = opened.lock().unwrap().clone().unwrap();
        assert_eq!(start.path(), "/oauth/google/start");
        let param = |k: &str| start.query_pairs().find(|(q, _)| q == k).map(|(_, v)| v.into_owned());
        assert_eq!((param("client").as_deref(), param("marketing").as_deref()), (Some("marv-os"), Some("1")));
        // The verifier sent at the end is the one the challenge in the browser was made from.
        let challenge = start.query_pairs().find(|(k, _)| k == "challenge").unwrap().1.into_owned();
        let finish = seen.lock().unwrap().iter().find(|r| r.path == "/oauth/finish").cloned().unwrap();
        let verifier = finish.body["verifier"].as_str().unwrap().to_string();
        let digest = ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes());
        assert_eq!(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest.as_ref()), challenge);
        assert_eq!(finish.body["device"], "Marv OS on ws");
    }

    #[tokio::test]
    async fn a_browser_refusal_says_why_and_signs_nothing_in() {
        let _one = BROWSER.lock().await;
        let (base, _) = stub(browser_server()).await;
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx(dir.path(), &base);
        let none = Arc::new(Mutex::new(None));
        let err = oauth_login(&ctx, "github", "d", false, browser("error=no_verified_email", none.clone())).await.unwrap_err();
        assert!(err.contains("no verified email"), "{err}");
        let err = oauth_login(&ctx, "github", "d", false, browser("error=cancelled", none)).await.unwrap_err();
        assert_eq!(err, "Sign-in was cancelled in the browser.");
        assert!(read_plan(dir.path()).is_none());
        assert!(oauth_login(&ctx, "facebook", "d", false, |_: &str| {}).await.is_err());
    }

    #[tokio::test]
    async fn cancel_stops_waiting_for_the_browser() {
        let _one = BROWSER.lock().await;
        let dir = tempfile::tempdir().unwrap();
        tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            cancel_sign_in();
        });
        let err = oauth_login(&ctx(dir.path(), "http://127.0.0.1:9"), "google", "d", false, |_: &str| {}).await.unwrap_err();
        assert_eq!(err, "Sign-in was cancelled.");
    }

    #[tokio::test]
    async fn sign_in_status_manage_and_sign_out_against_the_server() {
        let (base, seen) = stub(server("pro_models", true)).await;
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx(dir.path(), &base);

        login_start(&ctx, "a@b.co").await.unwrap();
        let wrong = login_verify(&ctx, "a@b.co", "000000", "Marv OS on ws", false).await.unwrap_err();
        assert_eq!(wrong, "That code <b>isn't right</b>.", "the server's words as plain printable text");

        let (status, installed) = login_verify(&ctx, "a@b.co", "123 456", "Marv OS on ws", true).await.unwrap();
        // Who signs in, and the person's tick for product emails (only when they gave it: the wrong code went false).
        let verifies: Vec<Seen> = seen.lock().unwrap().iter().filter(|r| r.path == "/login/verify").cloned().collect();
        assert_eq!(
            (verifies[0].body["client"].as_str(), verifies[0].body["marketing"].as_bool()),
            (Some("marv-os"), Some(false))
        );
        assert_eq!(verifies[1].body["marketing"], true);
        assert_eq!(installed, Some(KeySetup::Installed));
        assert!(status.signed_in && status.online && status.key_in_use && !status.own_key);
        assert_eq!((status.effective.as_str(), status.licence), ("pro_models", "valid"));
        let allowance = status.allowance.clone().unwrap();
        assert_eq!((allowance.limit_usd, allowance.used_usd), (8.0, 1.25));
        assert_eq!(allowance.resets_at.as_deref(), Some("2026-12-01T00:00:00Z"));
        assert_eq!(
            allowance.allowed_models,
            ["z-ai/glm-5.3", "deepseek/deepseek-v4.1-flash"],
            "the page narrows its menu to these"
        );
        let verify =
            seen.lock().unwrap().iter().find(|s| s.path == "/login/verify" && s.body["code"] == "123456").cloned().unwrap();
        assert_eq!(
            verify.body,
            json!({ "email": "a@b.co", "code": "123456", "device": "Marv OS on ws", "client": "marv-os", "marketing": true })
        );

        let file = read_plan(dir.path()).unwrap();
        assert_eq!((file.token.as_str(), file.key_label.as_deref()), (TOKEN, Some("sk-or-v1-012…")));
        #[cfg(unix)] // Windows has no mode bits
        assert_eq!(std::fs::metadata(plan_path(dir.path())).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(config(dir.path()).api_key.as_deref(), Some(PLAN_KEY));
        assert!(!serde_json::to_string(&status).unwrap().contains(PLAN_KEY), "the key never reaches the page");

        // Offline (no call), from plan.json and the licence.
        let offline = super::status(&ctx, false).await;
        assert!(offline.signed_in && !offline.online && offline.has_models);
        assert!(!offline.key_in_use && !offline.own_key, "offline, which key is the plan's isn't claimed");
        assert_eq!(offline.effective, "pro_models");

        assert_eq!(portal(&ctx).await.unwrap(), "https://billing.stripe.com/p/session/x");

        assert_eq!(logout(&ctx).await.unwrap(), SignedOut { removed: true, notice: None }, "the plan's own key goes");
        assert_eq!(config(dir.path()).api_key, None);
        assert_eq!(read_plan(dir.path()), None);
        assert!(seen.lock().unwrap().iter().any(|s| s.path == "/logout" && s.method == "POST"));
        assert_eq!(super::status(&ctx, true).await, PlanStatus::signed_out());
    }

    #[tokio::test]
    async fn a_person_with_their_own_key_keeps_it_and_can_choose_the_plan() {
        let (base, _) = stub(server("pro_models", true)).await;
        let dir = tempfile::tempdir().unwrap();
        set_config(dir.path(), &FileConfig { api_key: Some("sk-or-v1-mine".into()), ..FileConfig::default() });
        let ctx = ctx(dir.path(), &base);
        let (status, installed) = login_verify(&ctx, "a@b.co", "123456", "d", false).await.unwrap();
        assert_eq!(installed, Some(KeySetup::OwnKey));
        assert!(status.own_key && !status.key_in_use);
        assert_eq!(config(dir.path()).api_key.as_deref(), Some("sk-or-v1-mine"));

        use_plan_key(&ctx).await.unwrap();
        assert_eq!(config(dir.path()).api_key.as_deref(), Some(PLAN_KEY));

        // The environment's key comes first: the page is told, nothing changes.
        let env = Ctx { keys: Keys { pasted: None, env: Some("sk-or-v1-env".into()) }, ..ctx };
        assert!(use_plan_key(&env).await.unwrap_err().contains("OPENROUTER_API_KEY"));

        // Signing out after they put their own key back leaves theirs.
        set_config(dir.path(), &FileConfig { api_key: Some("sk-or-v1-mine".into()), ..FileConfig::default() });
        assert_eq!(logout(&env).await.unwrap(), SignedOut { removed: false, notice: None });
        assert_eq!(config(dir.path()).api_key.as_deref(), Some("sk-or-v1-mine"));
    }

    #[tokio::test]
    async fn founding_has_no_key_and_offline_or_revoked_still_shows_the_licence() {
        let (base, _) = stub(server("founding", false)).await;
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx(dir.path(), &base);
        let (status, installed) = login_verify(&ctx, "a@b.co", "123456", "d", false).await.unwrap();
        assert_eq!(installed, None);
        assert!(!status.has_models && status.allowance.is_none() && !status.own_key);
        assert_eq!(status.effective, "founding");
        assert_eq!(ConfigStore::new(dir.path()).load().unwrap(), None, "no key, nothing written to the config");
        assert_eq!(read_plan(dir.path()).unwrap().key_label, None);

        // The server is gone: the licence still says founding.
        let gone = Ctx { base: "http://127.0.0.1:1".into(), ..ctx };
        let offline = super::status(&gone, true).await;
        assert_eq!((offline.effective.as_str(), offline.online), ("founding", false));
        assert_eq!(offline.notice.as_deref(), Some("Offline: showing what this computer knows."));
        assert_eq!(portal(&gone).await.unwrap_err(), OFFLINE);

        // A revoked token: told to sign in again.
        let mut file = read_plan(dir.path()).unwrap();
        file.token = "f".repeat(64);
        write_plan(dir.path(), &file).unwrap();
        let revoked = Ctx { base: base.clone(), ..gone };
        assert_eq!(super::status(&revoked, true).await.notice.as_deref(), Some(SIGNED_OUT_THERE));
        assert_eq!(portal(&revoked).await.unwrap_err(), SIGNED_OUT_THERE);

        // An expired licence, offline: Free, nothing locked or deleted.
        file.licence = licence("founding", NOW - 1);
        write_plan(dir.path(), &file).unwrap();
        let expired = super::status(&revoked, false).await;
        assert_eq!((expired.effective.as_str(), expired.licence, expired.signed_in), ("free", "expired", true));
        assert!(read_plan(dir.path()).is_some());
    }

    #[tokio::test]
    async fn offline_sign_out_keeps_a_key_that_shares_the_label_prefix() {
        let (base, _) = stub(server("pro_models", true)).await;
        let dir = tempfile::tempdir().unwrap();
        login_verify(&ctx(dir.path(), &base), "a@b.co", "123456", "d", false).await.unwrap();
        // The person's own key, with the same first 12 characters as the plan's.
        let theirs = "sk-or-v1-012ffffffffffffff";
        assert_eq!(key_label(theirs), read_plan(dir.path()).unwrap().key_label.unwrap());
        set_config(dir.path(), &FileConfig { api_key: Some(theirs.into()), ..FileConfig::default() });
        let gone = ctx(dir.path(), "http://127.0.0.1:1");
        assert!(!super::status(&gone, true).await.key_in_use);
        let out = logout(&gone).await.unwrap();
        assert_eq!(out, SignedOut { removed: false, notice: Some(KEY_LEFT.into()) });
        assert_eq!(config(dir.path()).api_key.as_deref(), Some(theirs));
        assert_eq!(read_plan(dir.path()), None, "signed out all the same");

        // Even the plan's exact key stays when the server can't confirm it.
        login_verify(&ctx(dir.path(), &base), "a@b.co", "123456", "d", false).await.unwrap();
        set_config(dir.path(), &FileConfig { api_key: Some(PLAN_KEY.into()), ..FileConfig::default() });
        assert_eq!(logout(&gone).await.unwrap().notice.as_deref(), Some(KEY_LEFT));
        assert_eq!(config(dir.path()).api_key.as_deref(), Some(PLAN_KEY));
    }

    #[tokio::test]
    async fn an_html_error_page_never_reaches_the_page() {
        let (base, _) = stub(Arc::new(|_: &Seen| (502, "<html><script>x</script></html>".into()))).await;
        let dir = tempfile::tempdir().unwrap();
        let err = login_start(&ctx(dir.path(), &base), "a@b.co").await.unwrap_err();
        assert_eq!(err, "The Marv plan server answered 502.");
        assert!(login_start(&ctx(dir.path(), &base), "nope").await.unwrap_err().contains("email"));
    }
}
