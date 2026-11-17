//! Who can do what (runtime-kernel §20.4, decision O4). New in marv-rs.
//!
//! The person's rules (talk to, read), their grants (work while you're away) and timers, in one view with two lenses:
//! **by rule or grant** (what each permits, its expiry, Revoke) and **by chat** ("acme-api may: talk to acme-web
//! (rule 3) · read acme-web (rule 4) · work while you're away ($0.40 of $2.00) · wake at 09:00 (timer 5)").
//!
//! It only shows and revokes: making or widening a permission stays where it is (the rule dialog, a y/N in the
//! terminal). Each line names what revokes it ([`Revoke`]); the host does the revoking with its own person-only
//! commands. Pure: the stores are read by the host.

use serde::{Deserialize, Serialize};

use crate::grant::{Grant, Spend};
use crate::identity::{AgentId, AgentKind, AgentRef, mesh_name_for};
use crate::mode::Mode;
use crate::policy::{Policy, Subject};
use crate::timer::{Repeat, Timer, TimerBy};

/// What a permission line's Revoke removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum Revoke {
    Rule(u64),
    Grant(u64),
    Timer(u64),
}

/// One thing a chat may do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Permission {
    pub text: String,
    pub revoke: Revoke,
}

/// A chat, as the person's view names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatPermissions {
    pub chat: String,
    pub name: String,
    pub may: Vec<Permission>,
}

/// A chat the host knows: its id, its title (the person's own words) and folder.
#[derive(Debug, Clone)]
pub struct KnownChat {
    pub id: AgentId,
    pub title: String,
    pub folder: std::path::PathBuf,
}

impl KnownChat {
    fn agent(&self) -> AgentRef {
        AgentRef {
            mesh_name: mesh_name_for(&self.folder, &self.id),
            id: Some(self.id.clone()),
            folder: self.folder.clone(),
            kind: AgentKind::Chat,
        }
    }

    fn name(&self) -> String {
        let title = crate::metrics::clean_name(&self.title);
        if title.is_empty() { mesh_name_for(&self.folder, &self.id) } else { title }
    }
}

fn subject_words(subject: &Subject) -> String {
    match subject {
        Subject::Folder(scope) => {
            let name =
                scope.path().file_name().map_or_else(|| scope.path().display().to_string(), |n| n.to_string_lossy().into());
            format!("the Marvs under {}", crate::metrics::clean_name(&name))
        }
        Subject::Agent(_) => "one chat".into(),
    }
}

fn dollars(micros: u64) -> String {
    format!("${:.2}", micros as f64 / 1e6)
}

fn clock(ms: u64, utc_offset_ms: i64) -> String {
    let local = (ms as i64 + utc_offset_ms).rem_euclid(crate::DAY_MS as i64) as u64;
    format!("{:02}:{:02}", local / 3_600_000, local / 60_000 % 60)
}

/// The by-chat lens: for each chat, what the live `rules`, `grants` and `timers` let it do at `now_ms`. `spend`: each
/// grant's spend (today's is taken). `utc_offset_ms`: the person's clock, for a timer's time.
pub fn by_chat(
    chats: &[KnownChat],
    rules: &[Policy],
    grants: &[Grant],
    spend: &std::collections::BTreeMap<crate::GrantId, Spend>,
    timers: &[Timer],
    now_ms: u64,
    utc_offset_ms: i64,
) -> Vec<ChatPermissions> {
    let mut out = Vec::new();
    for chat in chats {
        let me = chat.agent();
        let mut may = Vec::new();
        for rule in rules.iter().filter(|r| r.live_at(now_ms)) {
            let (from, to) = (rule.from.covers(&me), rule.to.covers(&me));
            // The other end (`other`), and whether this chat acts (its end may send or read) or is only acted on.
            let (other, acts) = match (from, to) {
                (true, _) => (&rule.to, true),
                (false, true) => (&rule.from, rule.mutual),
                (false, false) => continue,
            };
            let peers: Vec<String> =
                chats.iter().filter(|c| c.id != chat.id && other.covers(&c.agent())).map(KnownChat::name).collect();
            let whom = if peers.is_empty() { subject_words(other) } else { peers.join(", ") };
            let verb = match (rule.mode, acts) {
                (Mode::Read, true) => "read",
                (Mode::Read, false) => "be read by",
                (_, true) => "talk to",
                (_, false) => "be messaged by",
            };
            may.push(Permission { text: format!("{verb} {whom} ({})", rule.id), revoke: Revoke::Rule(rule.id.0) });
        }
        for grant in grants.iter().filter(|g| g.chat == chat.id && g.live_at(now_ms)) {
            let today = spend.get(&grant.id).map(|s| s.today(now_ms)).unwrap_or_default();
            may.push(Permission {
                text: format!(
                    "work while you're away ({} of {} today, {})",
                    dollars(today.usd_micros),
                    dollars(grant.cap.usd_micros),
                    grant.id
                ),
                revoke: Revoke::Grant(grant.id.0),
            });
        }
        for timer in timers.iter().filter(|t| t.chat == chat.id) {
            let when = match &timer.repeat {
                Repeat::Once => format!("wake at {}", clock(timer.at_ms, utc_offset_ms)),
                Repeat::Every { every_ms } if *every_ms == crate::DAY_MS => {
                    format!("wake daily at {}", clock(timer.at_ms, utc_offset_ms))
                }
                Repeat::Every { every_ms } => format!("wake every {} min", every_ms / 60_000),
                // The person's clock and zone, as they set it (review P3-1).
                Repeat::Daily { minute_of_day, tz } => {
                    format!(
                        "wake daily at {:02}:{:02} {}",
                        minute_of_day / 60,
                        minute_of_day % 60,
                        crate::metrics::clean_name(tz)
                    )
                }
            };
            let by = if timer.by == TimerBy::Chat { ", set by the chat" } else { "" };
            may.push(Permission { text: format!("{when} ({}{by})", timer.id), revoke: Revoke::Timer(timer.id.0) });
        }
        out.push(ChatPermissions { chat: chat.id.as_str().into(), name: chat.name(), may });
    }
    out
}

/// One chat's line: "acme-api may: talk to acme-web (rule 3) · ...", or that it may do nothing on its own.
pub fn line(chat: &ChatPermissions) -> String {
    if chat.may.is_empty() {
        return format!("{} asks you first for everything", chat.name);
    }
    format!("{} may: {}", chat.name, chat.may.iter().map(|p| p.text.as_str()).collect::<Vec<_>>().join(" · "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::FolderScope;
    use crate::policy::{PolicyId, RuleBy};
    use crate::{GrantBy, GrantId, SpendCap, TimerId};
    use std::path::Path;

    const API: &str = "554963ee-fd8e-4bec-a7dc-49051f090acc";
    const WEB: &str = "11111111-fd8e-4bec-a7dc-49051f090acc";

    fn scope(p: &str) -> FolderScope {
        FolderScope::new(Path::new(p), Path::new("/home/u")).unwrap()
    }

    fn rule(id: u64, mode: Mode, mutual: bool, expires: Option<u64>) -> Policy {
        Policy {
            id: PolicyId(id),
            from: Subject::Folder(scope("/p/acme")),
            to: Subject::Folder(scope("/p/acme")),
            mode,
            mutual,
            expires_at_ms: expires,
            by: RuleBy::Desktop,
            created_at_ms: 0,
            label: String::new(),
        }
    }

    #[test]
    fn by_chat_names_what_each_may_do_and_its_revoke() {
        let chats = [
            KnownChat { id: AgentId::new(API).unwrap(), title: "acme-api".into(), folder: "/p/acme/api".into() },
            KnownChat { id: AgentId::new(WEB).unwrap(), title: "acme-web".into(), folder: "/p/acme/web".into() },
        ];
        let rules = [rule(3, Mode::Send, true, None), rule(4, Mode::Read, true, None), rule(5, Mode::Send, true, Some(1))];
        let grant = Grant {
            id: GrantId(2),
            chat: AgentId::new(API).unwrap(),
            folder: scope("/p/acme/api"),
            expires_at_ms: None,
            cap: SpendCap { usd_micros: 2_000_000, tokens: 1 },
            by: GrantBy::Desktop,
            created_at_ms: 0,
            label: String::new(),
        };
        let now = 10 * crate::DAY_MS + 1000;
        let mut spend = Spend::default();
        spend.charge(now, Some(0.4), 0);
        let spends = [(GrantId(2), spend)].into_iter().collect();
        let timer_of = |chat: &str| Timer {
            id: TimerId(5),
            chat: AgentId::new(chat).unwrap(),
            by: TimerBy::Person,
            repeat: Repeat::Once,
            at_ms: 0,
            note: String::new(),
            grant: None,
            hop: 0,
            created_at_ms: 0,
        };
        let timer = Timer {
            id: TimerId(5),
            chat: AgentId::new(API).unwrap(),
            by: TimerBy::Person,
            repeat: Repeat::Every { every_ms: crate::DAY_MS },
            at_ms: 11 * crate::DAY_MS + 9 * 3_600_000,
            note: "secret plan".into(),
            grant: None,
            hop: 0,
            created_at_ms: 0,
        };
        let view = by_chat(&chats, &rules, &[grant], &spends, &[timer], now, 0);
        assert_eq!(
            line(&view[0]),
            "acme-api may: talk to acme-web (rule 3) · read acme-web (rule 4) · work while you're away ($0.40 of $2.00 today, \
             grant 2) · wake daily at 09:00 (timer 5)"
        );
        assert_eq!(view[0].may[2].revoke, Revoke::Grant(2));
        assert!(!line(&view[0]).contains("secret"), "a timer's words aren't shown here");
        assert_eq!(line(&view[1]), "acme-web may: talk to acme-api (rule 3) · read acme-api (rule 4)");
        let daily = Timer {
            id: TimerId(6),
            repeat: Repeat::Daily { minute_of_day: 9 * 60, tz: "Europe/Berlin".into() },
            ..timer_of(API)
        };
        let view = by_chat(&chats[..1], &[], &[], &Default::default(), &[daily], now, 0);
        assert_eq!(line(&view[0]), "acme-api may: wake daily at 09:00 Europe/Berlin (timer 6)");
        let nobody = by_chat(&chats[..1], &[], &[], &Default::default(), &[], now, 0);
        assert_eq!(line(&nobody[0]), "acme-api asks you first for everything");
        assert_eq!(nobody[0].chat, API);
    }
}
