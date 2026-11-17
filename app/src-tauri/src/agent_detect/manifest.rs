//! The rule engine: an agent's rules (a TOML manifest) evaluated over its screen. Ported from herdr 0.9.5's
//! `src/detect/manifest.rs` (Apache-2.0, see `HERDR-LICENSE` beside this file); changed by Marv:
//!
//! - Only the manifests bundled here (`include_str!`): no remote updates and no local overrides, so the matching
//!   herdr code (`manifest_update.rs`, the override paths, `explain` and its JSON) isn't ported.
//! - Keyed by Marv's agent ids (`marv_web::terminal_agents::AGENTS`), not herdr's `Agent` enum.
//! - The screen's lines are split on `\n` only: [`super::screen::Screen::text`] never writes `\r`.
//!
//! Each rule names a region of the screen (`whole_recent`, `bottom_non_empty_lines(12)`, `osc_title`…), gates of
//! `contains` (case-insensitive), `regex` and `line_regex` (any line matches) with nested `all`, `any` and `not`, a
//! priority and the state it means. The highest-priority match wins; none: the agent counts as idle (herdr's
//! "known agent idle fallback"), which [`super::Detector`] takes as "nothing seen" and decides from its output.

use std::collections::HashMap;
use std::sync::OnceLock;

use regex::Regex;
use serde::Deserialize;

/// What a screen says an agent is doing (herdr's `AgentState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Working,
    Blocked,
    Unknown,
}

/// The engine's reading of one screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Detection {
    pub state: State,
    /// The matching rule's id; none: nothing matched (the idle fallback).
    pub rule: Option<&'static str>,
    /// The screen shows something that says nothing about the state (Codex's transcript viewer, Claude Code's
    /// model picker): keep the last one.
    pub skip_state_update: bool,
    /// The rule's state is plainly on screen (a prompt box, a dialog), not inferred: an idle one isn't debounced.
    pub visible_idle: bool,
    pub visible_blocker: bool,
    pub visible_working: bool,
}

impl Detection {
    /// No rule matched: herdr's idle fallback.
    pub const FALLBACK: Detection = Detection {
        state: State::Idle,
        rule: None,
        skip_state_update: false,
        visible_idle: false,
        visible_blocker: false,
        visible_working: false,
    };
}

/// The screen and the terminal's OSC strings a rule can look at. Empty strings where there's none.
#[derive(Debug, Clone, Copy)]
pub struct DetectionInput<'a> {
    pub screen: &'a str,
    /// The last window title (OSC 0 or 2).
    pub osc_title: &'a str,
    /// The last progress report (OSC 9;4), without its `9;`.
    pub osc_progress: &'a str,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct AgentManifest {
    id: String,
    #[serde(rename = "version")]
    _version: Option<String>,
    min_engine_version: Option<u32>,
    #[serde(rename = "updated_at")]
    _updated_at: Option<String>,
    #[serde(default, rename = "aliases")]
    _aliases: Vec<String>,
    #[serde(default)]
    rules: Vec<ManifestRule>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct ManifestRule {
    id: String,
    state: Option<ManifestState>,
    #[serde(default)]
    priority: i32,
    #[serde(default = "default_region")]
    region: String,
    #[serde(default)]
    visible_idle: bool,
    #[serde(default)]
    visible_blocker: bool,
    #[serde(default)]
    visible_working: bool,
    #[serde(default)]
    skip_state_update: bool,
    #[serde(default)]
    all: Vec<ManifestGate>,
    #[serde(default)]
    any: Vec<ManifestGate>,
    #[serde(default, rename = "not")]
    not_gate: Vec<ManifestGate>,
    #[serde(default)]
    contains: Vec<String>,
    #[serde(default)]
    regex: Vec<String>,
    #[serde(default)]
    line_regex: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct ManifestGate {
    #[serde(default)]
    all: Vec<ManifestGate>,
    #[serde(default)]
    any: Vec<ManifestGate>,
    #[serde(default, rename = "not")]
    not_gate: Vec<ManifestGate>,
    #[serde(default)]
    contains: Vec<String>,
    #[serde(default)]
    regex: Vec<String>,
    #[serde(default)]
    line_regex: Vec<String>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ManifestState {
    Idle,
    Working,
    Blocked,
    Unknown,
}

impl From<ManifestState> for State {
    fn from(value: ManifestState) -> Self {
        match value {
            ManifestState::Idle => State::Idle,
            ManifestState::Working => State::Working,
            ManifestState::Blocked => State::Blocked,
            ManifestState::Unknown => State::Unknown,
        }
    }
}

fn default_region() -> String {
    "whole_recent".to_string()
}

#[derive(Debug)]
struct CompiledGate {
    all: Vec<CompiledGate>,
    any: Vec<CompiledGate>,
    not_gate: Vec<CompiledGate>,
    contains: Vec<String>,
    regex: Vec<Regex>,
    line_regex: Vec<Regex>,
}

#[derive(Debug)]
struct CompiledRule {
    id: &'static str,
    state: State,
    priority: i32,
    region: String,
    visible_idle: bool,
    visible_blocker: bool,
    visible_working: bool,
    skip_state_update: bool,
    gate: CompiledGate,
}

/// One agent's compiled rules, built once.
#[derive(Debug)]
pub struct Rules {
    rules: Vec<CompiledRule>,
}

/// The manifests Marv bundles, by Marv's agent id (herdr's file names: `cursor.toml` is Cursor Agent's,
/// `antigravity.toml` Antigravity's `agy`). Marv itself and Crush have none: they're seen from their output alone.
const BUNDLED_MANIFESTS: &[(&str, &str)] = &[
    ("claude-code", include_str!("manifests/claude.toml")),
    ("codex", include_str!("manifests/codex.toml")),
    ("opencode", include_str!("manifests/opencode.toml")),
    ("gemini", include_str!("manifests/gemini.toml")),
    ("pi", include_str!("manifests/pi.toml")),
    ("cursor-agent", include_str!("manifests/cursor.toml")),
    ("antigravity", include_str!("manifests/antigravity.toml")),
];

const MAX_RULES_PER_MANIFEST: usize = 128;
const MAX_GATE_DEPTH: usize = 8;
const MAX_TOTAL_GATES: usize = 512;
const MAX_MATCHERS_PER_GATE: usize = 32;
const MAX_TOTAL_MATCHERS: usize = 1024;
const MAX_MATCHER_CHARS: usize = 512;
const TOP_NON_EMPTY_LINES_ENGINE_VERSION: u32 = 3;
const MAX_TOP_REGION_LINE_COUNT: usize = u16::MAX as usize;

/// Agent `id`'s rules (a Marv agent id), compiled on first use; none for an agent without a manifest.
pub fn rules_for(id: &str) -> Option<&'static Rules> {
    static ALL: OnceLock<HashMap<&'static str, Rules>> = OnceLock::new();
    ALL.get_or_init(|| {
        BUNDLED_MANIFESTS
            .iter()
            .map(|(id, content)| {
                // Bundled and covered by a test: a broken one is a build mistake, not a person's.
                let rules = parse_manifest(content)
                    .and_then(|m| compile_manifest(&m))
                    .unwrap_or_else(|e| panic!("bundled {id} manifest is invalid: {e}"));
                (*id, rules)
            })
            .collect()
    })
    .get(id)
}

impl Rules {
    /// The highest-priority rule matching `input` (the first of equal ones), or the idle fallback.
    pub fn detect(&self, input: DetectionInput<'_>) -> Detection {
        let mut matched: Option<&CompiledRule> = None;
        for rule in &self.rules {
            if matched.is_some_and(|m| m.priority >= rule.priority) {
                continue;
            }
            let text = region(input, &rule.region);
            if compiled_gate_matches(&rule.gate, text, &text.to_lowercase()) {
                matched = Some(rule);
            }
        }
        let Some(rule) = matched else { return Detection::FALLBACK };
        Detection {
            state: rule.state,
            rule: Some(rule.id),
            skip_state_update: rule.skip_state_update,
            visible_idle: rule.visible_idle && rule.state == State::Idle,
            visible_blocker: rule.visible_blocker && rule.state == State::Blocked,
            visible_working: rule.visible_working && rule.state == State::Working,
        }
    }
}

fn parse_manifest(content: &str) -> Result<AgentManifest, String> {
    let manifest = toml::from_str::<AgentManifest>(content).map_err(|err| err.to_string())?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn validate_manifest(manifest: &AgentManifest) -> Result<(), String> {
    if manifest.id.trim().is_empty() {
        return Err("manifest id must not be empty".to_string());
    }
    if manifest.rules.is_empty() {
        return Err("manifest must contain at least one rule".to_string());
    }
    if manifest.rules.len() > MAX_RULES_PER_MANIFEST {
        return Err(format!("manifest contains {} rules, max is {MAX_RULES_PER_MANIFEST}", manifest.rules.len()));
    }
    let mut complexity = ManifestComplexity::default();
    for rule in &manifest.rules {
        if rule.id.trim().is_empty() {
            return Err("manifest rule id must not be empty".to_string());
        }
        if rule.skip_state_update {
            if rule.state != Some(ManifestState::Unknown) {
                return Err(format!("rule {} uses skip_state_update without state = \"unknown\"", rule.id));
            }
            if rule.visible_idle || rule.visible_blocker || rule.visible_working {
                return Err(format!("rule {} uses skip_state_update with visible state evidence", rule.id));
            }
        }
        validate_region_name(&rule.region).map_err(|err| format!("rule {} uses invalid region: {err}", rule.id))?;
        if rule.region.trim().starts_with("top_non_empty_lines(")
            && manifest.min_engine_version.is_some_and(|version| version < TOP_NON_EMPTY_LINES_ENGINE_VERSION)
        {
            return Err(format!(
                "rule {} uses top_non_empty_lines but min_engine_version is below {TOP_NON_EMPTY_LINES_ENGINE_VERSION}",
                rule.id
            ));
        }
        validate_gate(&manifest_gate_from_rule(rule), "rule", 0, &mut complexity)
            .map_err(|err| format!("rule {} has invalid matcher gates: {err}", rule.id))?;
    }
    Ok(())
}

#[derive(Default)]
struct ManifestComplexity {
    total_gates: usize,
    total_matchers: usize,
}

fn validate_gate(gate: &ManifestGate, context: &str, depth: usize, complexity: &mut ManifestComplexity) -> Result<(), String> {
    count_gate(gate, context, depth, complexity)?;
    if !gate_has_positive_matcher(gate) {
        return Err(format!("{context} must contain a positive matcher"));
    }
    for nested in &gate.all {
        validate_gate(nested, "all gate", depth + 1, complexity)?;
    }
    for nested in &gate.any {
        validate_gate(nested, "any gate", depth + 1, complexity)?;
    }
    for nested in &gate.not_gate {
        validate_not_gate(nested, depth + 1, complexity)?;
    }
    Ok(())
}

fn validate_not_gate(gate: &ManifestGate, depth: usize, complexity: &mut ManifestComplexity) -> Result<(), String> {
    count_gate(gate, "not gate", depth, complexity)?;
    if !gate_has_any_matcher(gate) {
        return Err("not gate must contain a matcher".to_string());
    }
    for nested in &gate.all {
        validate_gate(nested, "not all gate", depth + 1, complexity)?;
    }
    for nested in &gate.any {
        validate_gate(nested, "not any gate", depth + 1, complexity)?;
    }
    for nested in &gate.not_gate {
        validate_not_gate(nested, depth + 1, complexity)?;
    }
    Ok(())
}

/// A gate's depth, count, matcher limits and regexes (herdr's checks shared by `validate_gate` and
/// `validate_not_gate`).
fn count_gate(gate: &ManifestGate, context: &str, depth: usize, complexity: &mut ManifestComplexity) -> Result<(), String> {
    if depth > MAX_GATE_DEPTH {
        return Err(format!("{context} exceeds max gate depth {MAX_GATE_DEPTH}"));
    }
    complexity.total_gates += 1;
    if complexity.total_gates > MAX_TOTAL_GATES {
        return Err(format!("manifest exceeds max gate count {MAX_TOTAL_GATES}"));
    }
    let matcher_count = gate.contains.len() + gate.regex.len() + gate.line_regex.len();
    if matcher_count > MAX_MATCHERS_PER_GATE {
        return Err(format!("{context} has {matcher_count} direct matchers, max is {MAX_MATCHERS_PER_GATE}"));
    }
    complexity.total_matchers += matcher_count;
    if complexity.total_matchers > MAX_TOTAL_MATCHERS {
        return Err(format!("manifest exceeds max matcher count {MAX_TOTAL_MATCHERS}"));
    }
    if gate.contains.iter().chain(&gate.regex).chain(&gate.line_regex).any(|v| v.chars().count() > MAX_MATCHER_CHARS) {
        return Err(format!("{context} matcher exceeds max length {MAX_MATCHER_CHARS}"));
    }
    for pattern in gate.regex.iter().chain(&gate.line_regex) {
        Regex::new(pattern).map_err(|err| format!("{context} contains invalid pattern {pattern:?}: {err}"))?;
    }
    Ok(())
}

fn gate_has_positive_matcher(gate: &ManifestGate) -> bool {
    !gate.contains.is_empty()
        || !gate.regex.is_empty()
        || !gate.line_regex.is_empty()
        || !gate.all.is_empty()
        || !gate.any.is_empty()
}

fn gate_has_any_matcher(gate: &ManifestGate) -> bool {
    gate_has_positive_matcher(gate) || !gate.not_gate.is_empty()
}

fn validate_region_name(spec: &str) -> Result<(), String> {
    let trimmed = spec.trim();
    match trimmed {
        "whole_recent"
        | "after_last_prompt_marker"
        | "before_current_prompt_marker"
        | "whole_recent_without_current_prompt_marker"
        | "current_prompt_block_marker"
        | "after_current_prompt_block_marker"
        | "prompt_box_body"
        | "above_prompt_box"
        | "last_non_empty_above_prompt_box"
        | "after_last_horizontal_rule"
        | "osc_title"
        | "osc_progress" => Ok(()),
        _ if region_count(trimmed, "bottom_lines").is_some()
            || region_count(trimmed, "bottom_non_empty_lines").is_some()
            || top_region_count(trimmed).is_some() =>
        {
            Ok(())
        }
        _ => Err(trimmed.to_string()),
    }
}

fn manifest_gate_from_rule(rule: &ManifestRule) -> ManifestGate {
    ManifestGate {
        all: rule.all.clone(),
        any: rule.any.clone(),
        not_gate: rule.not_gate.clone(),
        contains: rule.contains.clone(),
        regex: rule.regex.clone(),
        line_regex: rule.line_regex.clone(),
    }
}

fn compile_manifest(manifest: &AgentManifest) -> Result<Rules, String> {
    let rules = manifest
        .rules
        .iter()
        .map(|rule| {
            let gate = compile_gate(&manifest_gate_from_rule(rule))
                .map_err(|err| format!("rule {} could not be compiled: {err}", rule.id))?;
            Ok(CompiledRule {
                // Compiled once per bundled manifest for the app's life: its ids can be `'static`.
                id: Box::leak(rule.id.clone().into_boxed_str()),
                state: rule.state.map(State::from).unwrap_or(State::Unknown),
                priority: rule.priority,
                region: rule.region.clone(),
                visible_idle: rule.visible_idle,
                visible_blocker: rule.visible_blocker,
                visible_working: rule.visible_working,
                skip_state_update: rule.skip_state_update,
                gate,
            })
        })
        .collect::<Result<_, String>>()?;
    Ok(Rules { rules })
}

fn compile_gate(gate: &ManifestGate) -> Result<CompiledGate, String> {
    let regexes = |patterns: &[String]| {
        patterns.iter().map(|p| Regex::new(p).map_err(|err| err.to_string())).collect::<Result<Vec<_>, _>>()
    };
    Ok(CompiledGate {
        all: gate.all.iter().map(compile_gate).collect::<Result<_, _>>()?,
        any: gate.any.iter().map(compile_gate).collect::<Result<_, _>>()?,
        not_gate: gate.not_gate.iter().map(compile_gate).collect::<Result<_, _>>()?,
        contains: gate.contains.iter().map(|needle| needle.to_lowercase()).collect(),
        regex: regexes(&gate.regex)?,
        line_regex: regexes(&gate.line_regex)?,
    })
}

fn compiled_gate_matches(gate: &CompiledGate, text: &str, lower_text: &str) -> bool {
    gate.contains.iter().all(|needle| lower_text.contains(needle))
        && gate.regex.iter().all(|regex| regex.is_match(text))
        && gate.line_regex.iter().all(|regex| text.lines().any(|line| regex.is_match(line)))
        && gate.all.iter().all(|nested| compiled_gate_matches(nested, text, lower_text))
        && (gate.any.is_empty() || gate.any.iter().any(|nested| compiled_gate_matches(nested, text, lower_text)))
        && !gate.not_gate.iter().any(|nested| compiled_gate_matches(nested, text, lower_text))
}

fn region<'a>(input: DetectionInput<'a>, spec: &str) -> &'a str {
    let trimmed = spec.trim();
    // OSC regions come from their own strings, not the screen.
    match trimmed {
        "osc_title" => return input.osc_title,
        "osc_progress" => return input.osc_progress,
        _ => {}
    }
    let content = input.screen;
    match trimmed {
        "whole_recent" => content,
        "after_last_prompt_marker" => after_last_prompt_marker(content),
        "before_current_prompt_marker" => before_current_prompt_marker(content),
        "whole_recent_without_current_prompt_marker" => whole_recent_without_current_prompt_marker(content),
        "current_prompt_block_marker" => current_prompt_block_marker(content).unwrap_or(""),
        "after_current_prompt_block_marker" => after_current_prompt_block_marker(content).unwrap_or(""),
        "prompt_box_body" => prompt_box_body(content).unwrap_or(""),
        "above_prompt_box" => above_prompt_box(content),
        "last_non_empty_above_prompt_box" => last_non_empty_line(above_prompt_box(content)),
        "after_last_horizontal_rule" => after_last_horizontal_rule(content),
        _ => {
            if let Some(count) = region_count(trimmed, "bottom_lines") {
                return bottom_lines(content, count);
            }
            if let Some(count) = region_count(trimmed, "bottom_non_empty_lines") {
                return bottom_non_empty_lines(content, count);
            }
            if let Some(count) = top_region_count(trimmed) {
                return top_non_empty_lines(content, count);
            }
            ""
        }
    }
}

fn region_count(spec: &str, name: &str) -> Option<usize> {
    spec.strip_prefix(name)
        .and_then(|rest| rest.strip_prefix('('))
        .and_then(|rest| rest.strip_suffix(')'))
        .and_then(|count| count.parse::<usize>().ok())
}

fn top_region_count(spec: &str) -> Option<usize> {
    let count = spec.strip_prefix("top_non_empty_lines")?.strip_prefix('(')?.strip_suffix(')')?;
    if count.starts_with('0') || !count.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    count.parse::<usize>().ok().filter(|count| *count <= MAX_TOP_REGION_LINE_COUNT)
}

fn bottom_lines(content: &str, count: usize) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(count);
    slice_from_line_index(content, &lines, start)
}

fn bottom_non_empty_lines(content: &str, count: usize) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let Some(start_index) =
        lines.iter().enumerate().rev().filter(|(_, line)| !line.trim().is_empty()).take(count).last().map(|(index, _)| index)
    else {
        return "";
    };
    slice_from_line_index(content, &lines, start_index)
}

fn top_non_empty_lines(content: &str, count: usize) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let Some(end_index) =
        lines.iter().enumerate().filter(|(_, line)| !line.trim().is_empty()).take(count).last().map(|(index, _)| index)
    else {
        return "";
    };
    let byte_offset = line_start_offset(content, &lines, end_index + 1);
    &content[..byte_offset]
}

fn after_last_prompt_marker(content: &str) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let Some(index) = lines.iter().rposition(|line| codex_prompt_line(line)) else {
        return content;
    };
    slice_from_line_index(content, &lines, index + 1)
}

fn before_current_prompt_marker(content: &str) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let Some(index) = current_codex_prompt_index(&lines) else {
        return content;
    };
    &content[..line_start_offset(content, &lines, index)]
}

fn whole_recent_without_current_prompt_marker(content: &str) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    if current_codex_prompt_index(&lines).is_some() { "" } else { content }
}

fn current_prompt_block_marker(content: &str) -> Option<&str> {
    let lines: Vec<&str> = content.lines().collect();
    let prompt_index = current_codex_prompt_index(&lines)?;
    lines[..prompt_index].iter().rev().find(|line| codex_block_marker_line(line)).copied()
}

fn after_current_prompt_block_marker(content: &str) -> Option<&str> {
    let lines: Vec<&str> = content.lines().collect();
    let prompt_index = current_codex_prompt_index(&lines)?;
    let block_index = lines[..prompt_index].iter().rposition(|line| codex_block_marker_line(line))?;
    Some(slice_from_line_index(content, &lines, block_index))
}

/// Codex's composer (`› `) with no response block drawn under it: the prompt it's at now.
fn current_codex_prompt_index(lines: &[&str]) -> Option<usize> {
    let prompt_index = lines.iter().rposition(|line| codex_prompt_line(line))?;
    if lines[prompt_index + 1..].iter().any(|line| codex_block_marker_line(line)) {
        return None;
    }
    Some(prompt_index)
}

fn codex_prompt_line(line: &str) -> bool {
    line == "›" || line.starts_with("› ")
}

fn codex_block_marker_line(line: &str) -> bool {
    line.starts_with('•') || line.starts_with('■') || line.starts_with('✗') || line.starts_with('✓')
}

fn prompt_box_body(content: &str) -> Option<&str> {
    let lines: Vec<&str> = content.lines().collect();
    let top = prompt_box_top_border_index(&lines)?;
    let start = line_start_offset(content, &lines, top + 1);
    let end_index = lines[top + 1..]
        .iter()
        .position(|line| is_horizontal_rule(line))
        .map(|relative| top + 1 + relative)
        .unwrap_or(lines.len());
    let end = line_start_offset(content, &lines, end_index);
    Some(&content[start.min(content.len())..end.min(content.len())])
}

fn above_prompt_box(content: &str) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let Some(top) = prompt_box_top_border_index(&lines) else {
        return content;
    };
    &content[..line_start_offset(content, &lines, top)]
}

fn after_last_horizontal_rule(content: &str) -> &str {
    let mut last_rule_end = 0usize;
    let mut offset = 0usize;
    for line in content.lines() {
        let next_offset = offset + line.len() + 1;
        if is_horizontal_rule(line) {
            last_rule_end = next_offset.min(content.len());
        }
        offset = next_offset;
    }
    &content[last_rule_end..]
}

fn last_non_empty_line(content: &str) -> &str {
    content.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or("")
}

/// The second-to-last horizontal rule: a prompt box's top border (its bottom is the last).
fn prompt_box_top_border_index(lines: &[&str]) -> Option<usize> {
    let mut border_count = 0;
    for index in (0..lines.len()).rev() {
        if is_horizontal_rule(lines[index]) {
            border_count += 1;
            if border_count == 2 {
                return Some(index);
            }
        }
    }
    None
}

/// A line of `─` (a label after at least three of them is still a rule: Claude Code names a session on its border).
fn is_horizontal_rule(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    let rule_chars = trimmed.chars().take_while(|&ch| ch == '─').count();
    if rule_chars == 0 {
        return false;
    }
    let rule_bytes = trimmed.char_indices().nth(rule_chars).map(|(index, _)| index).unwrap_or(trimmed.len());
    let suffix = trimmed[rule_bytes..].trim_start();
    suffix.is_empty() || rule_chars >= 3
}

fn slice_from_line_index<'a>(content: &'a str, lines: &[&str], index: usize) -> &'a str {
    &content[line_start_offset(content, lines, index)..]
}

fn line_start_offset(content: &str, lines: &[&str], index: usize) -> usize {
    lines[..index.min(lines.len())].iter().map(|line| line.len() + 1).sum::<usize>().min(content.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(agent: &str, screen: &str) -> Detection {
        rules_for(agent).unwrap().detect(DetectionInput { screen, osc_title: "", osc_progress: "" })
    }

    fn titled(agent: &str, screen: &str, osc_title: &str) -> Detection {
        rules_for(agent).unwrap().detect(DetectionInput { screen, osc_title, osc_progress: "" })
    }

    #[test]
    fn every_bundled_manifest_parses_compiles_and_names_a_marv_agent() {
        for (id, _) in BUNDLED_MANIFESTS {
            assert!(rules_for(id).is_some(), "{id}");
            assert!(marv_web::terminal_agents::find(id).is_some(), "{id} isn't an agent Marv OS runs");
        }
        assert!(rules_for("marv").is_none() && rules_for("crush").is_none());
    }

    #[test]
    fn validation_rejects_unknown_fields_bad_regions_and_regexes() {
        let manifest = |rule: &str| format!("id = \"t\"\n[[rules]]\nid = \"r\"\nstate = \"idle\"\n{rule}\n");
        assert!(parse_manifest(&manifest("contains = [\"x\"]")).is_ok());
        assert!(parse_manifest(&manifest("contains = [\"x\"]\nbogus = 1")).is_err());
        assert!(parse_manifest(&manifest("contains = [\"x\"]\nregion = \"middle\"")).is_err());
        assert!(parse_manifest(&manifest("regex = [\"(\"]")).is_err());
        assert!(parse_manifest(&manifest("region = \"whole_recent\"")).is_err(), "a rule needs a positive matcher");
        assert!(parse_manifest(&manifest("contains = [\"x\"]\nskip_state_update = true")).is_err());
        assert!(parse_manifest("id = \"t\"").is_err(), "a manifest needs rules");
    }

    /// Priorities, `not` exclusions, `line_regex` (any one line) and nested `any`/`all`.
    #[test]
    fn gates_and_priorities_decide_the_match() {
        let manifest = r#"
id = "t"
[[rules]]
id = "low"
state = "working"
priority = 10
contains = ["busy"]
[[rules]]
id = "high"
state = "blocked"
priority = 20
contains = ["busy"]
not = [{ contains = ["never mind"] }]
any = [{ line_regex = ['^\s*allow\?$'] }, { all = [{ contains = ["y/n"] }, { regex = ['(?m)^>'] }] }]
"#;
        let rules = compile_manifest(&parse_manifest(manifest).unwrap()).unwrap();
        let at = |screen: &str| rules.detect(DetectionInput { screen, osc_title: "", osc_progress: "" }).rule;
        assert_eq!(at("BUSY\n  allow?\n"), Some("high"), "contains is case-insensitive");
        assert_eq!(at("busy\nallow? maybe\n"), Some("low"), "line_regex is per line");
        assert_eq!(at("busy\n> go on? y/n\n"), Some("high"));
        assert_eq!(at("busy\n  allow?\nnever mind\n"), Some("low"), "not excludes");
        assert_eq!(at("nothing\n"), None);
        assert_eq!(rules.detect(DetectionInput { screen: "x", osc_title: "", osc_progress: "" }), Detection::FALLBACK);
    }

    #[test]
    fn regions_cut_the_screen_as_herdr_does() {
        let text = "a\n\nb\nc\n\n";
        assert_eq!(bottom_non_empty_lines(text, 2), "b\nc\n\n");
        assert_eq!(bottom_lines("a\nb\nc\n", 2), "b\nc\n");
        assert_eq!(top_non_empty_lines(text, 2), "a\n\nb\n");
        let boxed = "reply\n──────\n❯ hi\n──────\n  ? for shortcuts\n";
        assert_eq!(prompt_box_body(boxed), Some("❯ hi\n"));
        assert_eq!(above_prompt_box(boxed), "reply\n");
        assert_eq!(after_last_horizontal_rule(boxed), "  ? for shortcuts\n");
        let codex = "• Ran it\n› 1. Yes\n  2. No\n";
        assert_eq!(after_last_prompt_marker(codex), "  2. No\n");
        assert_eq!(before_current_prompt_marker(codex), "• Ran it\n");
        assert_eq!(whole_recent_without_current_prompt_marker("› hi\n• answer\n"), "› hi\n• answer\n");
    }

    // Screens as Marv's screen model gives them: visible rows, trailing spaces and blank rows trimmed.

    const CLAUDE_WORKING: &str = "\
> run the tests

⏺ Bash(cargo test)
  ⎿  Running…

✻ Simmering… (12s · ↓ 1.2k tokens · esc to interrupt)

────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────
  ⏵⏵ auto mode on (shift+tab to cycle)
";

    const CLAUDE_BLOCKED: &str = "\
> clean up

⏺ I'll remove the build folder.

────────────────────────────────────────────────────────────
 Bash command

   rm -rf build
   Remove the build output

 Do you want to proceed?
 ❯ 1. Yes
   2. Yes, and don't ask again for rm commands in /home/u/w
   3. No, and tell Claude what to do differently (esc)

 Esc to cancel · Tab to amend · ctrl+e to explain
";

    const CLAUDE_IDLE: &str = "\
> run the tests

⏺ All 42 tests pass.

────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────
  ? for shortcuts
";

    #[test]
    fn claude_code_working_blocked_and_idle() {
        let working = screen("claude-code", CLAUDE_WORKING);
        assert_eq!((working.state, working.rule), (State::Working, Some("live_turn_working")));
        let blocked = screen("claude-code", CLAUDE_BLOCKED);
        assert_eq!((blocked.state, blocked.rule), (State::Blocked, Some("bash_permission_prompt")));
        assert!(blocked.visible_blocker);
        let idle = screen("claude-code", CLAUDE_IDLE);
        assert_eq!((idle.state, idle.rule), (State::Idle, Some("live_prompt_box")));
        assert!(idle.visible_idle);
        // Its title's spinner says working before anything else.
        assert_eq!(titled("claude-code", CLAUDE_IDLE, "⠂ Running tests").state, State::Working);
    }

    const CODEX_WORKING: &str = "\
› run the tests

• Ran cargo test
  └ test result: ok. 42 passed

• Working (14s • esc to interrupt)


› Summarize recent commits

  ? for shortcuts                                       96% context left
";

    const CODEX_BLOCKED: &str = "\
› clean up

• I'll remove the build folder.

  Would you like to run the following command?

  $ rm -rf build

› 1. Yes, proceed (y)
  2. Yes, and don't ask again for this command (p)
  3. No, and tell Codex what to do differently (esc)

  Press enter to confirm or esc to cancel
";

    const CODEX_IDLE: &str = "\
› run the tests

• All 42 tests pass.

› Ask Codex to do anything

  ? for shortcuts                                       96% context left
";

    #[test]
    fn codex_working_blocked_and_idle() {
        let working = screen("codex", CODEX_WORKING);
        assert_eq!((working.state, working.rule), (State::Working, Some("screen_working_fallback")));
        let blocked = screen("codex", CODEX_BLOCKED);
        assert_eq!((blocked.state, blocked.rule), (State::Blocked, Some("live_strong_blocker")));
        // At its prompt with nothing on screen saying more: the fallback (no rule), and its title says idle.
        assert_eq!(screen("codex", CODEX_IDLE), Detection::FALLBACK);
        let idle = titled("codex", CODEX_IDLE, "w");
        assert_eq!((idle.state, idle.rule, idle.visible_idle), (State::Idle, Some("osc_title_idle"), true));
        assert_eq!(titled("codex", CODEX_IDLE, "⠹ w").state, State::Working);
        assert_eq!(titled("codex", CODEX_IDLE, "Action Required").state, State::Blocked);
    }

    const OPENCODE_WORKING: &str = "\
┃  run the tests

   Bash cargo test

┃
┃
┃  Build  Claude Sonnet 4.5 Anthropic
╹▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀
 ■■■■⬝⬝⬝⬝  esc interrupt                          ctrl+p commands
";

    const OPENCODE_BLOCKED: &str = "\
┃  clean up

┃  △ Permission required
┃    $ rm -rf build
┃
┃   Allow once   Allow always   Reject          ctrl+f fullscreen ⇆ select enter confirm
";

    const OPENCODE_IDLE: &str = "\
┃  run the tests

   All 42 tests pass.

┃
┃
┃  Build  Claude Sonnet 4.5 Anthropic
╹▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀
                                                  ctrl+p commands
";

    #[test]
    fn opencode_working_blocked_and_idle() {
        assert_eq!(screen("opencode", OPENCODE_WORKING).state, State::Working);
        let blocked = screen("opencode", OPENCODE_BLOCKED);
        assert_eq!((blocked.state, blocked.rule), (State::Blocked, Some("permission_required")));
        assert_eq!(screen("opencode", OPENCODE_IDLE), Detection::FALLBACK);
    }

    #[test]
    fn gemini_and_cursor_approvals_block() {
        let gemini = "╭──────╮\n│ ? Shell rm -rf build\n│ Allow execution of: 'rm'?\n│ ● 1. Allow once\n╰──────╯\n";
        assert_eq!(screen("gemini", gemini).state, State::Blocked);
        let cursor = "  Run this command?\n  rm -rf build\n  Waiting for approval...\n  → Run (once) (y)\n    Skip (esc or n)\n";
        assert_eq!(screen("cursor-agent", cursor).state, State::Blocked);
    }
}
