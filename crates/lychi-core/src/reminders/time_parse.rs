use chrono::{Duration, Local, NaiveTime};

/// Parse a natural language time expression into milliseconds since UNIX epoch.
///
/// Supported formats:
/// - Relative: "in 30 minutes", "in 2 hours", "in 1 day", "30m", "2h", "5m30s"
/// - Absolute: "at 5pm", "at 17:00", "at 5:30pm", "at 14:30"
/// - Future: "tomorrow 9am", "tomorrow at 5pm"
pub fn parse_reminder_time(input: &str) -> Option<u64> {
    let lower = input.trim().to_lowercase();

    // Try relative duration: "in 30 minutes", "in 2 hours", "in 1 day"
    if let Some(rest) = lower.strip_prefix("in ") {
        return parse_relative(rest.trim());
    }

    // Try compact duration: "30m", "2h", "5m30s", "1h30m"
    if looks_like_duration(&lower) {
        return parse_relative(&lower);
    }

    // Try "tomorrow [at] <time>"
    if let Some(rest) = lower.strip_prefix("tomorrow") {
        let rest = rest.trim();
        let rest = rest.strip_prefix("at ").unwrap_or(rest).trim();
        if let Some(time) = parse_time_of_day(rest) {
            let tomorrow = Local::now().date_naive() + Duration::days(1);
            let dt = tomorrow.and_time(time);
            let local = dt.and_local_timezone(Local).single()?;
            return Some(local.timestamp_millis() as u64);
        }
    }

    // Try "at <time>"
    if let Some(time) = lower
        .strip_prefix("at ")
        .and_then(|rest| parse_time_of_day(rest.trim()))
    {
        return Some(time_today_or_tomorrow(time));
    }

    // Try bare time: "5pm", "17:00", "5:30pm"
    if let Some(time) = parse_time_of_day(&lower) {
        return Some(time_today_or_tomorrow(time));
    }

    None
}

/// Parse relative duration: "30 minutes", "2 hours", "1 day", "30m", "2h", "5m30s"
fn parse_relative(input: &str) -> Option<u64> {
    let secs = parse_duration_secs(input)?;
    if secs == 0 {
        return None;
    }
    let now = Local::now();
    let future = now + Duration::seconds(secs as i64);
    Some(future.timestamp_millis() as u64)
}

/// Parse duration into seconds. Handles both natural ("30 minutes") and compact ("30m") forms.
///
/// Shared by reminders and timer/stopwatch handlers.
pub fn parse_duration_secs(input: &str) -> Option<u64> {
    let input = input.trim();

    // Try natural language: "<N> <unit>" or "<N> <unit> and <N> <unit>"
    if let Some(secs) = parse_natural_duration(input) {
        return Some(secs);
    }

    // Try compact format: "30m", "5m30s", "1h30m", "2h"
    parse_compact_duration(input)
}

/// Parse natural language duration: "30 minutes", "2 hours", "1 day", "1 hour and 30 minutes"
fn parse_natural_duration(input: &str) -> Option<u64> {
    let mut total: u64 = 0;
    let mut found = false;

    // Split on "and" to handle "1 hour and 30 minutes"
    for part in input.split("and") {
        let part = part.trim();
        let tokens: Vec<&str> = part.split_whitespace().collect();
        if tokens.len() < 2 {
            continue;
        }
        let n: u64 = tokens[0].parse().ok()?;
        let unit = tokens[1].trim_end_matches('s'); // "minutes" → "minute"
        let secs = match unit {
            "second" | "sec" => n,
            "minute" | "min" => n * 60,
            "hour" | "hr" => n * 3600,
            "day" => n * 86400,
            _ => continue,
        };
        total += secs;
        found = true;
    }

    if found { Some(total) } else { None }
}

/// Normalize verbose unit suffixes to single-char: "20mins" → "20m", "2hours" → "2h", "30secs" → "30s"
pub fn normalize_duration_units(input: &str) -> String {
    let mut s = input.to_string();
    // Order matters: longer suffixes first to avoid partial replacement
    for (long, short) in &[
        ("minutes", "m"),
        ("minute", "m"),
        ("mins", "m"),
        ("min", "m"),
        ("hours", "h"),
        ("hour", "h"),
        ("hrs", "h"),
        ("hr", "h"),
        ("seconds", "s"),
        ("second", "s"),
        ("secs", "s"),
        ("sec", "s"),
        ("days", "d"),
        ("day", "d"),
    ] {
        s = s.replace(long, short);
    }
    s
}

/// Parse compact duration: "30m", "5m30s", "1h30m", "2h", "20mins", "2hours"
fn parse_compact_duration(input: &str) -> Option<u64> {
    let normalized = normalize_duration_units(input);
    let mut total: u64 = 0;
    let mut num_buf = String::new();
    let mut found_unit = false;

    for ch in normalized.chars() {
        if ch.is_ascii_digit() {
            num_buf.push(ch);
        } else {
            if num_buf.is_empty() {
                continue; // skip non-digit non-unit chars (spaces etc.)
            }
            let n: u64 = num_buf.parse().ok()?;
            num_buf.clear();
            match ch {
                'h' => {
                    total += n * 3600;
                    found_unit = true;
                }
                'm' => {
                    total += n * 60;
                    found_unit = true;
                }
                's' => {
                    total += n;
                    found_unit = true;
                }
                'd' => {
                    total += n * 86400;
                    found_unit = true;
                }
                ' ' => {} // ignore spaces between number and unit
                _ => return None,
            }
        }
    }

    // Trailing number without unit
    if !num_buf.is_empty() {
        let n: u64 = num_buf.parse().ok()?;
        if found_unit {
            total += n; // trailing seconds
        } else {
            total += n * 60; // bare number = minutes
        }
    }

    if total == 0 { None } else { Some(total) }
}

/// Check if input looks like a compact duration (has digits + time unit).
fn looks_like_duration(input: &str) -> bool {
    let has_digit = input.chars().any(|c| c.is_ascii_digit());
    let has_unit = input.contains("min")
        || input.contains("sec")
        || input.contains("hour")
        || input.contains("hr")
        || input.contains("day")
        || (input.contains('h') && !input.contains("am") && !input.contains("pm"))
        || (input.contains('m') && !input.contains("am") && !input.contains("pm"))
        || (input.contains('s') && !input.contains("am") && !input.contains("pm"));
    has_digit && has_unit
}

/// Parse a time-of-day string: "5pm", "5:30pm", "17:00", "14:30", "9am", "9:30am"
fn parse_time_of_day(input: &str) -> Option<NaiveTime> {
    let input = input.trim();

    // "5pm", "5:30pm", "9am", "9:30am"
    let (stripped, is_pm, is_12h) = if let Some(s) = input.strip_suffix("pm") {
        (s.trim(), true, true)
    } else if let Some(s) = input.strip_suffix("am") {
        (s.trim(), false, true)
    } else if let Some(s) = input.strip_suffix("p.m.") {
        (s.trim(), true, true)
    } else if let Some(s) = input.strip_suffix("a.m.") {
        (s.trim(), false, true)
    } else {
        (input, false, false)
    };

    if let Some((h_str, m_str)) = stripped.split_once(':') {
        let h: u32 = h_str.trim().parse().ok()?;
        let m: u32 = m_str.trim().parse().ok()?;
        let h = if is_12h { to_24h(h, is_pm)? } else { h };
        NaiveTime::from_hms_opt(h, m, 0)
    } else {
        let h: u32 = stripped.parse().ok()?;
        if is_12h {
            let h = to_24h(h, is_pm)?;
            NaiveTime::from_hms_opt(h, 0, 0)
        } else if h <= 23 {
            // Bare number without am/pm — only valid as 24h if in range
            NaiveTime::from_hms_opt(h, 0, 0)
        } else {
            None
        }
    }
}

/// Convert 12-hour to 24-hour.
fn to_24h(h: u32, is_pm: bool) -> Option<u32> {
    if h == 0 || h > 12 {
        return None;
    }
    Some(if is_pm {
        if h == 12 { 12 } else { h + 12 }
    } else if h == 12 {
        0
    } else {
        h
    })
}

/// Schedule for today if the time hasn't passed yet, otherwise tomorrow.
fn time_today_or_tomorrow(time: NaiveTime) -> u64 {
    let now = Local::now();
    let today = now.date_naive().and_time(time);
    let local = today.and_local_timezone(Local).single();

    if let Some(dt) = local.filter(|dt| *dt > now) {
        return dt.timestamp_millis() as u64;
    }

    // Time already passed today — schedule for tomorrow
    let tomorrow = (now.date_naive() + Duration::days(1)).and_time(time);
    tomorrow
        .and_local_timezone(Local)
        .single()
        .map(|dt| dt.timestamp_millis() as u64)
        .unwrap_or_else(|| {
            // Fallback: shouldn't happen, but just add 24h
            (now + Duration::days(1)).timestamp_millis() as u64
        })
}

/// Format a due_at timestamp as a human-readable relative string.
pub fn format_relative(due_at: u64) -> String {
    let now = crate::db::now_millis();
    if due_at <= now {
        return "now".to_string();
    }
    let diff_secs = (due_at - now) / 1000;
    if diff_secs < 60 {
        format!("in {}s", diff_secs)
    } else if diff_secs < 3600 {
        let m = diff_secs / 60;
        let s = diff_secs % 60;
        if s > 0 {
            format!("in {m}m {s}s")
        } else {
            format!("in {m}m")
        }
    } else if diff_secs < 86400 {
        let h = diff_secs / 3600;
        let m = (diff_secs % 3600) / 60;
        if m > 0 {
            format!("in {h}h {m}m")
        } else {
            format!("in {h}h")
        }
    } else {
        let d = diff_secs / 86400;
        let h = (diff_secs % 86400) / 3600;
        if h > 0 {
            format!("in {d}d {h}h")
        } else {
            format!("in {d}d")
        }
    }
}

/// Format a due_at timestamp as absolute time for display.
pub fn format_absolute(due_at: u64) -> String {
    let secs = (due_at / 1000) as i64;
    let nanos = ((due_at % 1000) * 1_000_000) as u32;
    if let Some(dt) = chrono::DateTime::from_timestamp(secs, nanos) {
        let local = dt.with_timezone(&Local);
        let now = Local::now();
        if local.date_naive() == now.date_naive() {
            // Today — just show time
            local.format("today %I:%M %p").to_string()
        } else if local.date_naive() == now.date_naive() + Duration::days(1) {
            local.format("tomorrow %I:%M %p").to_string()
        } else {
            local.format("%b %d %I:%M %p").to_string()
        }
    } else {
        "unknown".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_minutes() {
        let result = parse_reminder_time("in 30 minutes").unwrap();
        let now = crate::db::now_millis();
        // Should be ~30 minutes from now (within 2 seconds tolerance)
        let diff = result - now;
        assert!((diff as i64 - 30 * 60 * 1000).unsigned_abs() < 2000);
    }

    #[test]
    fn relative_hours() {
        let result = parse_reminder_time("in 2 hours").unwrap();
        let now = crate::db::now_millis();
        let diff = result - now;
        assert!((diff as i64 - 2 * 3600 * 1000).unsigned_abs() < 2000);
    }

    #[test]
    fn relative_compound() {
        let result = parse_reminder_time("in 1 hour and 30 minutes").unwrap();
        let now = crate::db::now_millis();
        let diff = result - now;
        assert!((diff as i64 - 90 * 60 * 1000).unsigned_abs() < 2000);
    }

    #[test]
    fn compact_duration() {
        let result = parse_reminder_time("30m").unwrap();
        let now = crate::db::now_millis();
        let diff = result - now;
        assert!((diff as i64 - 30 * 60 * 1000).unsigned_abs() < 2000);
    }

    #[test]
    fn compact_combined() {
        let result = parse_reminder_time("1h30m").unwrap();
        let now = crate::db::now_millis();
        let diff = result - now;
        assert!((diff as i64 - 90 * 60 * 1000).unsigned_abs() < 2000);
    }

    #[test]
    fn absolute_time() {
        // This test just checks it parses successfully — exact timestamp depends on local time
        let result = parse_reminder_time("at 5pm");
        assert!(result.is_some());
        // Should be today or tomorrow at 5pm
        let ts = result.unwrap();
        assert!(ts > crate::db::now_millis().saturating_sub(86400 * 1000));
    }

    #[test]
    fn absolute_time_with_minutes() {
        let result = parse_reminder_time("at 5:30pm");
        assert!(result.is_some());
    }

    #[test]
    fn absolute_24h() {
        let result = parse_reminder_time("at 17:00");
        assert!(result.is_some());
    }

    #[test]
    fn tomorrow_time() {
        let result = parse_reminder_time("tomorrow 9am");
        assert!(result.is_some());
        let ts = result.unwrap();
        let now = crate::db::now_millis();
        // Should be at least ~12 hours from now (at most ~36 hours)
        assert!(ts > now);
        assert!(ts - now < 36 * 3600 * 1000);
    }

    #[test]
    fn tomorrow_at_time() {
        let result = parse_reminder_time("tomorrow at 5pm");
        assert!(result.is_some());
    }

    #[test]
    fn verbose_units() {
        // "20mins", "2hrs", "30secs"
        let result = parse_reminder_time("20mins").unwrap();
        let now = crate::db::now_millis();
        let diff = result - now;
        assert!((diff as i64 - 20 * 60 * 1000).unsigned_abs() < 2000);

        let result = parse_reminder_time("2hrs").unwrap();
        let diff = result - now;
        assert!((diff as i64 - 2 * 3600 * 1000).unsigned_abs() < 2000);

        let result = parse_reminder_time("30secs").unwrap();
        let diff = result - now;
        assert!((diff as i64 - 30 * 1000).unsigned_abs() < 2000);

        // "in 20mins"
        let result = parse_reminder_time("in 20mins").unwrap();
        let diff = result - now;
        assert!((diff as i64 - 20 * 60 * 1000).unsigned_abs() < 2000);
    }

    #[test]
    fn invalid_input() {
        assert!(parse_reminder_time("").is_none());
        assert!(parse_reminder_time("hello world").is_none());
        assert!(parse_reminder_time("in").is_none());
    }

    #[test]
    fn format_relative_display() {
        let now = crate::db::now_millis();
        assert_eq!(format_relative(now), "now");
        assert_eq!(format_relative(now + 30_000), "in 30s");
        assert_eq!(format_relative(now + 5 * 60_000), "in 5m");
        assert_eq!(format_relative(now + 90 * 60_000), "in 1h 30m");
    }
}
