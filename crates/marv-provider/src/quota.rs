//! A plan's quota, as its provider's response headers tell it (runtime-kernel §16.5). New in marv-rs.
//!
//! Read from a successful response's headers before its body: how much of the plan's window is left and when it
//! resets. Only numbers, never a header's other text. Headers seen:
//! - OpenAI's style: `x-ratelimit-remaining-requests` / `-tokens` (a whole number) and `x-ratelimit-reset-requests`
//!   / `-tokens` (a duration, `1s`, `6m0s`, `1h2m3.5s`), and the plain `x-ratelimit-remaining` / `x-ratelimit-reset`
//!   and the IETF draft's `ratelimit-remaining` / `ratelimit-reset` (seconds from now, or a Unix time in seconds).
//! - ChatGPT's (the Codex backend): `x-codex-primary-used-percent` and `x-codex-primary-reset-after-seconds`.

use marv_core::Quota;
use reqwest::header::HeaderMap;

/// A duration in OpenAI's header form (`6m0s`, `1.5s`, `120ms`, or plain seconds) in milliseconds.
fn duration_ms(text: &str) -> Option<u64> {
    let text = text.trim();
    if let Ok(seconds) = text.parse::<f64>() {
        return (seconds.is_finite() && seconds >= 0.0).then_some((seconds * 1000.0) as u64);
    }
    let (mut total, mut number) = (0.0f64, String::new());
    let mut chars = text.chars().peekable();
    let mut any = false;
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() || c == '.' {
            number.push(c);
            continue;
        }
        let value: f64 = number.parse().ok()?;
        number.clear();
        let unit = match c {
            'h' => 3_600_000.0,
            'm' if chars.peek() == Some(&'s') => {
                chars.next();
                1.0
            }
            'm' => 60_000.0,
            's' => 1000.0,
            _ => return None,
        };
        total += value * unit;
        any = true;
    }
    (any && number.is_empty() && total.is_finite()).then_some(total as u64)
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok()).map(str::trim).filter(|v| !v.is_empty())
}

/// A reset as `ratelimit-reset` may say it: seconds from now, or (a number past a year's seconds) a Unix time.
fn reset_at(text: &str, now_ms: u64) -> Option<u64> {
    let seconds: u64 = text.parse().ok()?;
    if seconds > 365 * 24 * 3600 {
        Some(seconds.saturating_mul(1000))
    } else {
        Some(now_ms.saturating_add(seconds.saturating_mul(1000)))
    }
}

/// The quota in a response's headers at `now_ms`, or `None` when they say nothing about one.
pub fn from_headers(headers: &HeaderMap, now_ms: u64) -> Option<Quota> {
    let number = |name: &str| header(headers, name).and_then(|v| v.parse::<u64>().ok());
    let remaining = number("x-ratelimit-remaining-requests")
        .or_else(|| number("x-ratelimit-remaining-tokens"))
        .or_else(|| number("x-ratelimit-remaining"))
        .or_else(|| number("ratelimit-remaining"));
    let reset_at_ms = header(headers, "x-ratelimit-reset-requests")
        .or_else(|| header(headers, "x-ratelimit-reset-tokens"))
        .and_then(duration_ms)
        .map(|ms| now_ms.saturating_add(ms))
        .or_else(|| {
            header(headers, "x-ratelimit-reset").or_else(|| header(headers, "ratelimit-reset")).and_then(|v| reset_at(v, now_ms))
        })
        .or_else(|| {
            header(headers, "x-codex-primary-reset-after-seconds")
                .and_then(|v| v.parse::<u64>().ok())
                .map(|s| now_ms.saturating_add(s.saturating_mul(1000)))
        });
    let used_percent = header(headers, "x-codex-primary-used-percent")
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|p| p.is_finite() && *p >= 0.0)
        .map(|p| p.round().min(100.0) as u32);
    (remaining.is_some() || reset_at_ms.is_some() || used_percent.is_some()).then_some(Quota {
        remaining,
        used_percent,
        reset_at_ms,
    })
}

/// Now, in ms since the Unix epoch.
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            map.insert(*k, v.parse().unwrap());
        }
        map
    }

    #[test]
    fn openai_codex_and_ietf_headers_are_read_as_numbers() {
        let openai =
            from_headers(&headers(&[("x-ratelimit-remaining-requests", "59"), ("x-ratelimit-reset-requests", "6m0s")]), 1000);
        assert_eq!(openai, Some(Quota { remaining: Some(59), used_percent: None, reset_at_ms: Some(361_000) }));
        let codex = from_headers(
            &headers(&[("x-codex-primary-used-percent", "12.6"), ("x-codex-primary-reset-after-seconds", "3600")]),
            0,
        );
        assert_eq!(codex, Some(Quota { remaining: None, used_percent: Some(13), reset_at_ms: Some(3_600_000) }));
        let ietf = from_headers(&headers(&[("ratelimit-remaining", "3"), ("ratelimit-reset", "1800000000")]), 5);
        assert_eq!(ietf, Some(Quota { remaining: Some(3), used_percent: None, reset_at_ms: Some(1_800_000_000_000) }));
        assert_eq!(from_headers(&headers(&[("x-ratelimit-reset-tokens", "1h2m3.5s")]), 0).unwrap().reset_at_ms, Some(3_723_500));
        assert_eq!(from_headers(&headers(&[("x-ratelimit-reset-tokens", "120ms")]), 0).unwrap().reset_at_ms, Some(120));
        assert_eq!(from_headers(&headers(&[("x-ratelimit-remaining", "lots")]), 0), None, "only numbers");
        assert_eq!(from_headers(&HeaderMap::new(), 0), None);
    }
}
