//! A server asking the person before it answers: MCP's multi round-trip requests (the 2026-07-28 draft's
//! `InputRequiredResult`). New in marv-rs; Marv had none.
//!
//! - **Why:** Google's Workspace servers (Gmail, Calendar, Drive, since 2026-11-24) ask before an agent reads mail
//!   from outside your organization or makes a change. A client that doesn't say it can ask gets the content withheld
//!   (`omittedContent`, `DATA_PROTECTION_POLICY`) or the write refused.
//! - **Saying so:** each `tools/call` carries `_meta["io.modelcontextprotocol/clientCapabilities"]` with form
//!   elicitation ([`client_capabilities`]). Not `initialize`'s capabilities: a stateful server could then send
//!   `elicitation/create` over a stream, and Marv answers no server request but ping.
//! - **The question:** an `InputRequiredResult` comes back in place of the result, either as the draft writes it
//!   (`resultType: "input_required"`, `inputRequests`, `requestState`) or as Google wraps it in
//!   `structuredContent.inputRequiredResult` (`elicitRequest`, `enumSchema`). Only a choice is understood here (one
//!   field, a list of values): Google's confirmations ("Allow once", "Always allow this tool", "Don't allow"). A form
//!   with other fields is refused with the server's own question, quoted.
//! - **The answer:** the call is sent again, under a new id, with `inputResponses` (an `ElicitResult` per request,
//!   `action: "accept"` and the chosen value) and `requestState` echoed back untouched.
//! - The question's text is the server's, and can quote an email's subject (an outsider's words): it's made
//!   printable and capped before the person sees it, and never reaches the model as an instruction.

use serde_json::{Map, Value, json};

use crate::text::{printable, truncate};

/// The `_meta` key a request declares the client's capabilities under (the draft's stateless requests).
pub const CAPABILITIES_META: &str = "io.modelcontextprotocol/clientCapabilities";
/// A question longer than this is cut (it can quote an email subject).
const MAX_MESSAGE: usize = 400;

/// The revision a single request says it's in (`_meta`), for servers that keep no handshake.
pub const PROTOCOL_META: &str = "io.modelcontextprotocol/protocolVersion";
/// The revision with confirmations as results (`resultType: "input_required"`), as Google's servers speak it.
pub const STATELESS_PROTOCOL: &str = "2026-07-28";

/// What every `tools/call` declares: form elicitation, answered through Marv's approval prompt.
pub fn client_capabilities() -> Value {
    json!({ "elicitation": { "form": {} } })
}

/// One choice the server offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// What goes back (`ALLOW_ONCE`).
    pub value: String,
    /// What the person reads ("Allow once").
    pub label: String,
}

/// One question: pick one of [`Confirmation::choices`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirmation {
    /// The request's key in `inputRequests`.
    pub key: String,
    /// The field the choice is the value of.
    pub field: String,
    /// The server's question, printable and capped.
    pub message: String,
    /// The field's title ("Allow this app to use search_threads for this email?"), printable.
    pub title: Option<String>,
    pub choices: Vec<Choice>,
    /// Google's shape: its answers go back wrapped (`elicitResult`) as well as plain.
    google: bool,
}

/// The server wants answers before it gives the result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputRequired {
    pub confirmations: Vec<Confirmation>,
    /// Echoed back exactly, never read.
    pub request_state: Option<String>,
}

/// What the retry carries: `inputResponses` and the echoed `requestState`.
#[derive(Debug, Clone, PartialEq)]
pub struct Answers {
    pub responses: Map<String, Value>,
    pub request_state: Option<String>,
}

impl Answers {
    /// The retry's params: the call's own, plus the answers.
    pub fn add_to(&self, params: &mut Map<String, Value>) {
        params.insert("inputResponses".into(), Value::Object(self.responses.clone()));
        if let Some(state) = &self.request_state {
            params.insert("requestState".into(), Value::String(state.clone()));
        }
    }
}

impl InputRequired {
    /// The answers, one chosen value per confirmation (by key). A confirmation with no value chosen is declined.
    pub fn answer(&self, chosen: &[(String, Option<String>)]) -> Answers {
        let mut responses = Map::new();
        for c in &self.confirmations {
            let value = chosen.iter().find(|(k, _)| k == &c.key).and_then(|(_, v)| v.clone());
            let result = match value {
                Some(v) => json!({ "action": "accept", "content": { c.field.clone(): v } }),
                None => json!({ "action": "decline" }),
            };
            // Google wraps each request in `elicitRequest`; its answer is read from `elicitResult` there, from the
            // plain ElicitResult elsewhere. Both are sent: unknown fields are ignored (the draft's error handling).
            let response = if c.google {
                let mut both = result.as_object().cloned().unwrap_or_default();
                both.insert("elicitResult".into(), result);
                Value::Object(both)
            } else {
                result
            };
            responses.insert(c.key.clone(), response);
        }
        Answers { responses, request_state: self.request_state.clone() }
    }
}

/// The question in a `tools/call` result, if it is one. `Err`: the server asks for something Marv can't show (a
/// form with free-text fields, a sampling request), with its question for the model to pass on.
pub fn input_required(result: &Value) -> Option<Result<InputRequired, String>> {
    let (body, google) = match result.get("structuredContent").and_then(|s| s.get("inputRequiredResult")) {
        Some(wrapped) => (wrapped, true),
        None => {
            let kind = result.get("resultType").and_then(Value::as_str)?;
            if !matches!(kind, "input_required" | "inputRequired" | "RESULT_TYPE_INPUT_REQUIRED") {
                return None;
            }
            (result, false)
        }
    };
    let request_state = body.get("requestState").and_then(Value::as_str).map(str::to_string);
    let requests = body.get("inputRequests").and_then(Value::as_object);
    let mut confirmations = Vec::new();
    for (key, request) in requests.into_iter().flatten() {
        match confirmation(key, request, google) {
            Some(c) => confirmations.push(c),
            None => return Some(Err(unsupported(request))),
        }
    }
    if confirmations.is_empty() && request_state.is_none() {
        return Some(Err("the server asked for input but sent no question".into()));
    }
    Some(Ok(InputRequired { confirmations, request_state }))
}

/// The server's question for a request Marv can't show, quoted.
fn unsupported(request: &Value) -> String {
    match message_of(request) {
        Some(m) => format!("the server asks for input Marv can't show yet: \"{m}\""),
        None => "the server asks for input Marv can't show yet".into(),
    }
}

fn message_of(request: &Value) -> Option<String> {
    let params = request.get("elicitRequest").or_else(|| request.get("params")).unwrap_or(request);
    let text = params.get("message")?.as_str()?;
    Some(truncate(&printable(text.trim()), MAX_MESSAGE))
}

/// One request as a choice: an elicitation whose schema has exactly one field, and that field a list of values.
fn confirmation(key: &str, request: &Value, google: bool) -> Option<Confirmation> {
    let (params, wrapped) = match request.get("elicitRequest") {
        Some(p) => (p, true),
        None if request.get("method").and_then(Value::as_str) == Some("elicitation/create") => (request.get("params")?, false),
        None => return None,
    };
    if !wrapped && params.get("mode").and_then(Value::as_str).is_some_and(|m| m != "form") {
        return None;
    }
    let schema = params.get("requestedSchema")?.as_object()?;
    // The draft: a JSON Schema object. Google: the fields themselves, by name.
    let fields = match schema.get("properties").and_then(Value::as_object) {
        Some(props) => props,
        None => schema,
    };
    let [(field, spec)] = fields.iter().collect::<Vec<_>>()[..] else { return None };
    let (title, choices) = choices_of(spec)?;
    Some(Confirmation {
        key: key.to_string(),
        field: field.clone(),
        message: message_of(request).unwrap_or_default(),
        title: title.map(|t| truncate(&printable(t.trim()), MAX_MESSAGE)),
        choices,
        google: google || wrapped,
    })
}

/// A field's values and their labels: Google's `enumSchema` (`enumList`, `enumNames`), or JSON Schema's `enum`
/// (with `enumNames`) or `oneOf` of `const`/`title`.
fn choices_of(spec: &Value) -> Option<(Option<&str>, Vec<Choice>)> {
    let label = |value: &str, name: Option<&Value>| Choice {
        value: value.to_string(),
        label: truncate(&printable(name.and_then(Value::as_str).unwrap_or(value)), 80),
    };
    if let Some(e) = spec.get("enumSchema") {
        let values = e.get("enumList")?.as_array()?;
        let names = e.get("enumNames").and_then(Value::as_array);
        let choices =
            values.iter().enumerate().filter_map(|(i, v)| Some(label(v.as_str()?, names.and_then(|n| n.get(i))))).collect();
        return Some((e.get("title").and_then(Value::as_str), choices)).filter(|(_, c): &(_, Vec<Choice>)| !c.is_empty());
    }
    let title = spec.get("title").and_then(Value::as_str);
    let choices: Vec<Choice> = if let Some(values) = spec.get("enum").and_then(Value::as_array) {
        let names = spec.get("enumNames").and_then(Value::as_array);
        values.iter().enumerate().filter_map(|(i, v)| Some(label(v.as_str()?, names.and_then(|n| n.get(i))))).collect()
    } else {
        let options = spec.get("oneOf").or_else(|| spec.get("anyOf"))?.as_array()?;
        options.iter().filter_map(|o| Some(label(o.get("const")?.as_str()?, o.get("title")))).collect()
    };
    (!choices.is_empty()).then_some((title, choices))
}

/// Which of a confirmation's choices means "once", "always for this tool" and "no", when the server uses Google's
/// values. `None` where it doesn't offer one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Meanings {
    pub once: Option<String>,
    pub always: Option<String>,
    pub no: Option<String>,
}

impl Confirmation {
    /// Google's `ALLOW_ONCE` / `ALLOW_TOOL_ALWAYS` / `DENY_ONCE`, or a plain allow/deny pair (`allow`, `deny`,
    /// `yes`, `no`, `accept`, `decline`). Anything else has no meanings, and isn't shown as a yes/no.
    pub fn meanings(&self) -> Meanings {
        let find = |names: &[&str]| {
            self.choices.iter().find(|c| names.iter().any(|n| c.value.eq_ignore_ascii_case(n))).map(|c| c.value.clone())
        };
        Meanings {
            once: find(&["ALLOW_ONCE", "allow", "yes", "accept", "approve"]),
            always: find(&["ALLOW_TOOL_ALWAYS"]),
            no: find(&["DENY_ONCE", "deny", "no", "decline", "reject"]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn google() -> Value {
        json!({
            "content": [{"type": "text", "text": "{…}"}],
            "structuredContent": {"inputRequiredResult": {
                "inputRequests": {"approval_choice": {
                    "elicitRequest": {
                        "message": "This agent wants to read **\"Hi\"** from an external user. Do you want to allow this?",
                        "requestedSchema": {"approval_choice": {"enumSchema": {
                            "enumList": ["ALLOW_ONCE", "ALLOW_RESOURCE_ALWAYS", "ALLOW_TOOL_ALWAYS", "DENY_ONCE"],
                            "enumNames": ["Allow once", "Always allow for this email", "Always allow this tool", "Don't allow"],
                            "singleSelect": {"defaultItem": "DENY_ONCE"},
                            "title": "Allow this app to use search_threads for this email?"
                        }}}
                    },
                    "requiredFields": ["approval_choice"]
                }},
                "metadata": {"isConfirmation": true, "title": "Use this data?"},
                "requestState": "{\"elicitationId\":\"e/1\"}",
                "resultType": "RESULT_TYPE_INPUT_REQUIRED"
            }}
        })
    }

    #[test]
    fn googles_confirmation_is_a_choice_with_its_meanings() {
        let ir = input_required(&google()).unwrap().unwrap();
        assert_eq!(ir.request_state.as_deref(), Some("{\"elicitationId\":\"e/1\"}"));
        let [c] = &ir.confirmations[..] else { panic!("one confirmation") };
        assert_eq!((c.key.as_str(), c.field.as_str()), ("approval_choice", "approval_choice"));
        assert!(c.message.starts_with("This agent wants to read"));
        assert_eq!(c.title.as_deref(), Some("Allow this app to use search_threads for this email?"));
        assert_eq!(c.choices[1], Choice { value: "ALLOW_RESOURCE_ALWAYS".into(), label: "Always allow for this email".into() });
        let m = c.meanings();
        assert_eq!(
            (m.once.as_deref(), m.always.as_deref(), m.no.as_deref()),
            (Some("ALLOW_ONCE"), Some("ALLOW_TOOL_ALWAYS"), Some("DENY_ONCE"))
        );
    }

    #[test]
    fn the_answer_echoes_the_state_and_goes_back_in_both_shapes_for_google() {
        let ir = input_required(&google()).unwrap().unwrap();
        let answers = ir.answer(&[("approval_choice".into(), Some("ALLOW_ONCE".into()))]);
        let mut params = Map::new();
        params.insert("name".into(), json!("search_threads"));
        answers.add_to(&mut params);
        let plain = json!({"action": "accept", "content": {"approval_choice": "ALLOW_ONCE"}});
        assert_eq!(params["inputResponses"]["approval_choice"]["elicitResult"], plain);
        assert_eq!(params["inputResponses"]["approval_choice"]["action"], "accept");
        assert_eq!(params["requestState"], "{\"elicitationId\":\"e/1\"}");
        // Nothing chosen: declined.
        let none = ir.answer(&[]);
        assert_eq!(none.responses["approval_choice"]["action"], "decline");
    }

    #[test]
    fn the_drafts_shape_is_understood_and_answered_plainly() {
        let result = json!({
            "resultType": "input_required",
            "inputRequests": {"ok": {"method": "elicitation/create", "params": {
                "mode": "form", "message": "Delete the file?",
                "requestedSchema": {"type": "object", "properties": {"choice": {"type": "string", "enum": ["yes", "no"]}}, "required": ["choice"]}
            }}},
            "requestState": "s1"
        });
        let ir = input_required(&result).unwrap().unwrap();
        let c = &ir.confirmations[0];
        assert_eq!(
            (c.message.as_str(), c.meanings().once.as_deref(), c.meanings().no.as_deref()),
            ("Delete the file?", Some("yes"), Some("no"))
        );
        let answers = ir.answer(&[("ok".into(), Some("yes".into()))]);
        assert_eq!(answers.responses["ok"], json!({"action": "accept", "content": {"choice": "yes"}}));
    }

    #[test]
    fn a_form_marv_cant_show_is_refused_with_its_question_and_a_plain_result_is_none() {
        let form = json!({
            "resultType": "input_required",
            "inputRequests": {"who": {"method": "elicitation/create", "params": {
                "mode": "form", "message": "Your GitHub username\u{1b}[31m?",
                "requestedSchema": {"type": "object", "properties": {"name": {"type": "string"}}}
            }}}
        });
        let err = input_required(&form).unwrap().unwrap_err();
        assert!(err.contains("Your GitHub username") && !err.contains('\u{1b}'), "{err}");
        assert!(input_required(&json!({"content": [{"type": "text", "text": "hi"}]})).is_none());
        assert!(input_required(&json!({"structuredContent": {"threads": []}})).is_none());
    }
}
