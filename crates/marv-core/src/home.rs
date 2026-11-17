//! The person's home folder from an environment, with Windows' fallback.
//!
//! `HOME` is a Unix variable: on Windows it is normally unset (the home is `%USERPROFILE%`), and code that read only
//! `HOME` fell back to `/` there: the Home-chat guards never fired, `~/x` expanded to `\x`, Marv's bash ran with
//! `HOME=/`, and the ChatGPT sign-in had no folder (audit, 2026-11). One helper, so every caller agrees with the
//! desktop (`dirs::home_dir`, which is `%USERPROFILE%` on Windows).

use std::path::PathBuf;

/// The home folder from `get` (a variable lookup): `HOME` when set and non-empty, else on Windows `USERPROFILE`, else
/// `HOMEDRIVE` + `HOMEPATH`. `None` when none of them gives one (callers pick their own last resort). Pure, so it is
/// tested everywhere; [`home_from_process_env`] and [`home_from_map`] apply it.
pub fn home_from<F>(get: F, windows: bool) -> Option<PathBuf>
where
    F: Fn(&str) -> Option<String>,
{
    let set = |name: &str| get(name).filter(|v| !v.is_empty());
    if let Some(home) = set("HOME") {
        return Some(PathBuf::from(home));
    }
    if !windows {
        return None;
    }
    if let Some(profile) = set("USERPROFILE") {
        return Some(PathBuf::from(profile));
    }
    match (set("HOMEDRIVE"), set("HOMEPATH")) {
        (Some(drive), Some(path)) => Some(PathBuf::from(format!("{drive}{path}"))),
        _ => None,
    }
}

/// [`home_from`] on Marv's own environment, for this OS.
pub fn home_from_process_env() -> Option<PathBuf> {
    home_from(|k| std::env::var_os(k).and_then(|v| v.into_string().ok()), cfg!(windows))
}

/// [`home_from`] on an environment map (a session's `Env`), for this OS.
pub fn home_from_map(env: &std::collections::BTreeMap<String, String>) -> Option<PathBuf> {
    home_from(|k| env.get(k).cloned(), cfg!(windows))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
    }

    #[test]
    fn home_wins_and_an_empty_home_counts_as_unset() {
        assert_eq!(home_from(env(&[("HOME", "/h"), ("USERPROFILE", r"C:\u")]), true), Some("/h".into()));
        assert_eq!(home_from(env(&[("HOME", ""), ("USERPROFILE", r"C:\u")]), true), Some(r"C:\u".into()));
    }

    #[test]
    fn windows_falls_back_to_userprofile_then_homedrive_and_homepath() {
        assert_eq!(home_from(env(&[("USERPROFILE", r"C:\Users\me")]), true), Some(r"C:\Users\me".into()));
        assert_eq!(home_from(env(&[("HOMEDRIVE", "C:"), ("HOMEPATH", r"\Users\me")]), true), Some(r"C:\Users\me".into()));
        assert_eq!(home_from(env(&[("HOMEDRIVE", "C:")]), true), None);
    }

    #[test]
    fn elsewhere_only_home_counts() {
        assert_eq!(home_from(env(&[("USERPROFILE", "/x")]), false), None);
        assert_eq!(home_from(env(&[]), true), None);
    }
}
