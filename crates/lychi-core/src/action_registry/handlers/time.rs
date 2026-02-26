use async_trait::async_trait;
use chrono::{Datelike, Local, NaiveTime, Offset, TimeZone, Timelike, Utc};
use chrono_tz::Tz;

use crate::action_registry::{ActionHandler, ActionResult, CompletionItem, OutputType, RiskLevel};
use crate::error::LychiError;

pub struct TimeHandler;

impl TimeHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TimeHandler {
    fn default() -> Self {
        Self::new()
    }
}

/// City/alias → IANA timezone name.
/// Sorted alphabetically for binary search in completions.
const CITY_MAP: &[(&str, &str, &str)] = &[
    // (lookup_key, display_name, IANA timezone)
    ("amsterdam", "Amsterdam", "Europe/Amsterdam"),
    ("ankara", "Ankara", "Europe/Istanbul"),
    ("auckland", "Auckland", "Pacific/Auckland"),
    ("bangalore", "Bangalore", "Asia/Kolkata"),
    ("bangkok", "Bangkok", "Asia/Bangkok"),
    ("beijing", "Beijing", "Asia/Shanghai"),
    ("berlin", "Berlin", "Europe/Berlin"),
    ("bogota", "Bogota", "America/Bogota"),
    (
        "buenos aires",
        "Buenos Aires",
        "America/Argentina/Buenos_Aires",
    ),
    ("cairo", "Cairo", "Africa/Cairo"),
    ("cape town", "Cape Town", "Africa/Johannesburg"),
    ("chicago", "Chicago", "America/Chicago"),
    ("dallas", "Dallas", "America/Chicago"),
    ("delhi", "Delhi", "Asia/Kolkata"),
    ("denver", "Denver", "America/Denver"),
    ("dhaka", "Dhaka", "Asia/Dhaka"),
    ("dubai", "Dubai", "Asia/Dubai"),
    ("dublin", "Dublin", "Europe/Dublin"),
    ("hawaii", "Hawaii", "Pacific/Honolulu"),
    ("helsinki", "Helsinki", "Europe/Helsinki"),
    ("hong kong", "Hong Kong", "Asia/Hong_Kong"),
    ("honolulu", "Honolulu", "Pacific/Honolulu"),
    ("houston", "Houston", "America/Chicago"),
    ("istanbul", "Istanbul", "Europe/Istanbul"),
    ("jakarta", "Jakarta", "Asia/Jakarta"),
    ("johannesburg", "Johannesburg", "Africa/Johannesburg"),
    ("karachi", "Karachi", "Asia/Karachi"),
    ("kolkata", "Kolkata", "Asia/Kolkata"),
    ("kuala lumpur", "Kuala Lumpur", "Asia/Kuala_Lumpur"),
    ("la", "Los Angeles", "America/Los_Angeles"),
    ("lagos", "Lagos", "Africa/Lagos"),
    ("lisbon", "Lisbon", "Europe/Lisbon"),
    ("london", "London", "Europe/London"),
    ("los angeles", "Los Angeles", "America/Los_Angeles"),
    ("madrid", "Madrid", "Europe/Madrid"),
    ("manila", "Manila", "Asia/Manila"),
    ("melbourne", "Melbourne", "Australia/Melbourne"),
    ("mexico city", "Mexico City", "America/Mexico_City"),
    ("miami", "Miami", "America/New_York"),
    ("moscow", "Moscow", "Europe/Moscow"),
    ("mumbai", "Mumbai", "Asia/Kolkata"),
    ("nairobi", "Nairobi", "Africa/Nairobi"),
    ("new york", "New York", "America/New_York"),
    ("ny", "New York", "America/New_York"),
    ("nyc", "New York", "America/New_York"),
    ("osaka", "Osaka", "Asia/Tokyo"),
    ("oslo", "Oslo", "Europe/Oslo"),
    ("paris", "Paris", "Europe/Paris"),
    ("perth", "Perth", "Australia/Perth"),
    ("rome", "Rome", "Europe/Rome"),
    ("san francisco", "San Francisco", "America/Los_Angeles"),
    ("santiago", "Santiago", "America/Santiago"),
    ("sao paulo", "Sao Paulo", "America/Sao_Paulo"),
    ("seattle", "Seattle", "America/Los_Angeles"),
    ("seoul", "Seoul", "Asia/Seoul"),
    ("sf", "San Francisco", "America/Los_Angeles"),
    ("shanghai", "Shanghai", "Asia/Shanghai"),
    ("singapore", "Singapore", "Asia/Singapore"),
    ("stockholm", "Stockholm", "Europe/Stockholm"),
    ("sydney", "Sydney", "Australia/Sydney"),
    ("taipei", "Taipei", "Asia/Taipei"),
    ("tokyo", "Tokyo", "Asia/Tokyo"),
    ("toronto", "Toronto", "America/Toronto"),
    ("vancouver", "Vancouver", "America/Vancouver"),
    ("vienna", "Vienna", "Europe/Vienna"),
    ("warsaw", "Warsaw", "Europe/Warsaw"),
    ("washington", "Washington DC", "America/New_York"),
    ("zurich", "Zurich", "Europe/Zurich"),
];

/// Common timezone abbreviations → IANA name.
/// Ambiguous ones use the most common interpretation.
const TZ_ABBREVS: &[(&str, &str, &str)] = &[
    // (abbreviation, display, IANA timezone)
    ("acst", "ACST", "Australia/Adelaide"),
    ("aest", "AEST", "Australia/Sydney"),
    ("akst", "AKST", "America/Anchorage"),
    ("ast", "AST", "America/Halifax"),
    ("awst", "AWST", "Australia/Perth"),
    ("bst", "BST", "Europe/London"),
    ("cat", "CAT", "Africa/Harare"),
    ("cet", "CET", "Europe/Paris"),
    ("cst", "CST", "America/Chicago"),
    ("ct", "CT", "America/Chicago"),
    ("eat", "EAT", "Africa/Nairobi"),
    ("eet", "EET", "Europe/Helsinki"),
    ("est", "EST", "America/New_York"),
    ("et", "ET", "America/New_York"),
    ("gmt", "GMT", "Etc/GMT"),
    ("hkt", "HKT", "Asia/Hong_Kong"),
    ("hst", "HST", "Pacific/Honolulu"),
    ("ist", "IST", "Asia/Kolkata"),
    ("jst", "JST", "Asia/Tokyo"),
    ("kst", "KST", "Asia/Seoul"),
    ("msk", "MSK", "Europe/Moscow"),
    ("mst", "MST", "America/Denver"),
    ("mt", "MT", "America/Denver"),
    ("nzst", "NZST", "Pacific/Auckland"),
    ("pht", "PHT", "Asia/Manila"),
    ("pkt", "PKT", "Asia/Karachi"),
    ("pst", "PST", "America/Los_Angeles"),
    ("pt", "PT", "America/Los_Angeles"),
    ("sgt", "SGT", "Asia/Singapore"),
    ("utc", "UTC", "Etc/UTC"),
    ("wat", "WAT", "Africa/Lagos"),
    ("wet", "WET", "Europe/Lisbon"),
];

/// Resolve a timezone query to (display_name, Tz).
fn resolve_tz(query: &str) -> Option<(&'static str, Tz)> {
    let lower = query.trim().to_lowercase();

    // 1. Try city map
    for &(key, display, iana) in CITY_MAP {
        if key == lower {
            return Some((display, iana.parse::<Tz>().ok()?));
        }
    }

    // 2. Try abbreviation map
    for &(abbr, display, iana) in TZ_ABBREVS {
        if abbr == lower {
            return Some((display, iana.parse::<Tz>().ok()?));
        }
    }

    // 3. Try raw IANA name (e.g. "America/Chicago") — case-sensitive
    if let Ok(tz) = query.trim().parse::<Tz>() {
        // Use the IANA name itself as display
        let name = tz.name();
        // Extract city part for display: "America/New_York" → "New York"
        let display = name.rsplit('/').next().unwrap_or(name).replace('_', " ");
        // We leak a tiny string here — fine for a launcher, called rarely
        let display_leaked: &'static str = Box::leak(display.into_boxed_str());
        return Some((display_leaked, tz));
    }

    None
}

/// Parse a time string like "3pm", "3:30pm", "14:00", "noon", "midnight", "now".
fn parse_time(s: &str) -> Option<NaiveTime> {
    let s = s.trim().to_lowercase();

    if s == "now" {
        return Some(Local::now().time());
    }
    if s == "noon" || s == "12pm" {
        return NaiveTime::from_hms_opt(12, 0, 0);
    }
    if s == "midnight" || s == "12am" {
        return NaiveTime::from_hms_opt(0, 0, 0);
    }

    // Try "3pm", "3am", "3:30pm", "3:30am"
    let (time_part, is_pm) = if let Some(t) = s.strip_suffix("pm") {
        (t.trim(), true)
    } else if let Some(t) = s.strip_suffix("am") {
        (t.trim(), false)
    } else if let Some(t) = s.strip_suffix("p") {
        (t.trim(), true)
    } else if let Some(t) = s.strip_suffix("a") {
        (t.trim(), false)
    } else {
        // Try 24h format: "14:00", "9:30"
        if let Some((h, m)) = time_part_24h(&s) {
            return NaiveTime::from_hms_opt(h, m, 0);
        }
        return None;
    };

    let (hour, minute) = if let Some(pos) = time_part.find(':') {
        let h: u32 = time_part[..pos].parse().ok()?;
        let m: u32 = time_part[pos + 1..].parse().ok()?;
        (h, m)
    } else {
        let h: u32 = time_part.parse().ok()?;
        (h, 0)
    };

    if hour > 12 || minute > 59 {
        return None;
    }

    let hour_24 = if is_pm {
        if hour == 12 { 12 } else { hour + 12 }
    } else if hour == 12 {
        0
    } else {
        hour
    };

    NaiveTime::from_hms_opt(hour_24, minute, 0)
}

/// Parse 24h time like "14:00", "9:30".
fn time_part_24h(s: &str) -> Option<(u32, u32)> {
    let pos = s.find(':')?;
    let h: u32 = s[..pos].parse().ok()?;
    let m: u32 = s[pos + 1..].parse().ok()?;
    if h < 24 && m < 60 { Some((h, m)) } else { None }
}

/// Format a UTC offset like "+5:30" or "-8:00" from a chrono_tz::Tz.
fn format_utc_offset(tz: Tz) -> String {
    let now = Utc::now().with_timezone(&tz);
    let offset = now.offset().fix();
    format_fixed_offset(offset)
}

/// Format a FixedOffset as "UTC+5:30" etc.
fn format_fixed_offset(offset: chrono::FixedOffset) -> String {
    let total_secs = offset.local_minus_utc();
    let sign = if total_secs >= 0 { "+" } else { "-" };
    let abs = total_secs.unsigned_abs();
    let hours = abs / 3600;
    let minutes = (abs % 3600) / 60;
    if minutes == 0 {
        format!("UTC{sign}{hours}")
    } else {
        format!("UTC{sign}{hours}:{minutes:02}")
    }
}

/// Format time in 12h AM/PM format.
fn format_time_12h(time: &impl Timelike) -> String {
    let hour = time.hour();
    let minute = time.minute();
    let (h, ampm) = if hour == 0 {
        (12, "AM")
    } else if hour < 12 {
        (hour, "AM")
    } else if hour == 12 {
        (12, "PM")
    } else {
        (hour - 12, "PM")
    };
    format!("{h}:{minute:02} {ampm}")
}

/// Get the short timezone abbreviation from a chrono DateTime.
fn tz_abbrev(tz: Tz) -> String {
    let now = Utc::now().with_timezone(&tz);
    now.format("%Z").to_string()
}

/// Check if a timezone is currently observing DST.
/// Compares current offset to the offset on Jan 1 (standard time in most zones).
fn is_dst(tz: Tz) -> bool {
    let now = Utc::now().with_timezone(&tz);
    let current_offset = now.offset().fix().local_minus_utc();

    // Check offset on Jan 1 of the current year (standard time for northern hemisphere)
    // and Jul 1 (standard time for southern hemisphere) — the one with the smaller
    // absolute offset is standard time.
    let year = now.year();
    let jan1 = tz
        .with_ymd_and_hms(year, 1, 1, 12, 0, 0)
        .single()
        .map(|dt| dt.offset().fix().local_minus_utc());
    let jul1 = tz
        .with_ymd_and_hms(year, 7, 1, 12, 0, 0)
        .single()
        .map(|dt| dt.offset().fix().local_minus_utc());

    match (jan1, jul1) {
        (Some(jan), Some(jul)) => {
            // If offsets are the same, no DST in this zone
            if jan == jul {
                return false;
            }
            // Standard offset is the smaller one (less ahead of UTC)
            let standard = jan.min(jul);
            current_offset != standard
        }
        _ => false,
    }
}

/// Execute a world clock query: show current time in a timezone.
fn world_clock(query: &str) -> Result<String, String> {
    let (display, tz) =
        resolve_tz(query).ok_or_else(|| format!("Unknown timezone or city: '{query}'"))?;
    let now = Utc::now().with_timezone(&tz);
    let time_str = format_time_12h(&now);
    let date_str = now.format("%a %b %-d").to_string();
    let abbr = tz_abbrev(tz);
    let offset = format_utc_offset(tz);
    let dst = if is_dst(tz) { " · DST" } else { "" };
    Ok(format!(
        "{display} ({abbr}): {time_str}, {date_str} · {offset}{dst}"
    ))
}

/// Execute a timezone conversion: "3pm EST to IST".
fn convert_time(input: &str) -> Result<String, String> {
    // Split on " to "
    let parts: Vec<&str> = input.splitn(2, " to ").collect();
    if parts.len() != 2 {
        return Err("Use format: <time> <timezone> to <timezone>".to_string());
    }

    let left = parts[0].trim();
    let target_str = parts[1].trim();

    // Parse target timezone
    let (target_display, target_tz) =
        resolve_tz(target_str).ok_or_else(|| format!("Unknown target timezone: '{target_str}'"))?;

    // Parse left side: "<time> <source_tz>" or just "<source_tz>" (implies now)
    // Try to split: last word might be the source timezone
    let (time_str, source_str) = split_time_and_tz(left)?;

    let (source_display, source_tz) =
        resolve_tz(source_str).ok_or_else(|| format!("Unknown source timezone: '{source_str}'"))?;

    // Parse time
    let naive_time =
        parse_time(time_str).ok_or_else(|| format!("Can't parse time: '{time_str}'"))?;

    // Create source datetime (today in source timezone, at parsed time)
    let source_today = Utc::now().with_timezone(&source_tz).date_naive();
    let source_dt = source_today
        .and_time(naive_time)
        .and_local_timezone(source_tz)
        .single()
        .ok_or("Ambiguous or invalid time in source timezone")?;

    // Convert to target
    let target_dt = source_dt.with_timezone(&target_tz);

    let source_time_str = format_time_12h(&source_dt);
    let target_time_str = format_time_12h(&target_dt);
    let source_abbr = source_dt.format("%Z").to_string();
    let target_abbr = target_dt.format("%Z").to_string();

    // Check if day changed
    let day_diff = target_dt
        .date_naive()
        .signed_duration_since(source_dt.date_naive())
        .num_days();
    let day_note = match day_diff {
        0 => String::new(),
        1 => " (+1 day)".to_string(),
        -1 => " (-1 day)".to_string(),
        n => format!(" ({n:+} days)"),
    };

    let source_dst = if is_dst(source_tz) { " DST" } else { "" };
    let target_dst = if is_dst(target_tz) { " DST" } else { "" };
    Ok(format!(
        "{source_time_str} {source_display} ({source_abbr}{source_dst}) → {target_time_str} {target_display} ({target_abbr}{target_dst}){day_note}"
    ))
}

/// Split "3pm EST" into ("3pm", "EST") or "now EST" into ("now", "EST").
/// If there's no time part, treat the whole thing as a timezone with "now".
fn split_time_and_tz(input: &str) -> Result<(&str, &str), String> {
    let parts: Vec<&str> = input.rsplitn(2, ' ').collect();
    if parts.len() == 2 {
        // parts[0] is the last word (potential tz), parts[1] is the rest (potential time)
        let maybe_tz = parts[0];
        let maybe_time = parts[1];
        // Verify the last word is a timezone
        if resolve_tz(maybe_tz).is_some() {
            return Ok((maybe_time, maybe_tz));
        }
    }
    // Single word — could be just a timezone (use "now")
    if parts.len() == 1 && resolve_tz(parts[0]).is_some() {
        return Ok(("now", parts[0]));
    }
    Err(format!(
        "Can't parse: '{input}'. Use format: <time> <timezone> to <timezone>"
    ))
}

/// Check if input looks like a timezone conversion (contains " to " with timezone-like tokens).
pub fn is_tz_conversion(input: &str) -> bool {
    let lower = input.to_lowercase();
    let Some(to_pos) = lower.find(" to ") else {
        return false;
    };

    let target = lower[to_pos + 4..].trim();
    // Target must be a known timezone/city
    if resolve_tz(target).is_none() {
        return false;
    }

    // Left side must end with a known timezone/city
    let left = lower[..to_pos].trim();
    let last_word = left.rsplit(' ').next().unwrap_or(left);
    resolve_tz(last_word).is_some()
}

#[async_trait]
impl ActionHandler for TimeHandler {
    fn id(&self) -> &str {
        "time"
    }

    fn description(&self) -> &str {
        "World clock & timezone conversion"
    }

    fn default_risk(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: &str) -> Result<ActionResult, LychiError> {
        let input = args.trim();
        let start = std::time::Instant::now();

        let result = if input.is_empty() {
            // No args: show local time + UTC
            let local = Local::now();
            let utc = Utc::now();
            let local_str = format_time_12h(&local);
            let utc_str = format_time_12h(&utc);
            let date_str = local.format("%a %b %-d").to_string();
            let local_offset = format_fixed_offset(local.offset().fix());
            Ok(format!(
                "Local: {local_str}, {date_str} · {local_offset}\nUTC: {utc_str}"
            ))
        } else if input.to_lowercase().contains(" to ") && is_tz_conversion(input) {
            // Conversion mode: "3pm EST to IST"
            convert_time(input)
        } else {
            // World clock: "tokyo" or "EST"
            world_clock(input)
        };

        let duration_ms = start.elapsed().as_millis() as u64;

        match result {
            Ok(output) => Ok(ActionResult {
                success: true,
                output: Some(output),
                error: None,
                duration_ms,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: Some(OutputType::Status),
                executed_args: None,
            }),
            Err(e) => Ok(ActionResult {
                success: false,
                output: None,
                error: Some(e),
                duration_ms,
                routed_by: None,
                open_url: None,
                needs_confirmation: None,
                risk_level: None,
                output_type: None,
                executed_args: None,
            }),
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let lower = partial.trim().to_lowercase();
        let mut items = Vec::new();

        // Conversion expressions ("3pm EST to IST") — no completions needed,
        // the user already typed a full query. Showing partial matches on
        // substrings like "ist" or "ct" from the input is misleading.
        if lower.contains(" to ") {
            return items;
        }

        // Show popular cities when empty
        if lower.is_empty() {
            let popular = [
                "new york",
                "london",
                "tokyo",
                "sydney",
                "paris",
                "berlin",
                "dubai",
                "singapore",
                "los angeles",
                "mumbai",
            ];
            for city in popular {
                if let Some(&(_, display, iana)) = CITY_MAP.iter().find(|&&(k, _, _)| k == city)
                    && let Ok(tz) = iana.parse::<Tz>()
                {
                    let now = Utc::now().with_timezone(&tz);
                    let time_str = format_time_12h(&now);
                    let abbr = tz_abbrev(tz);
                    let dst_tag = if is_dst(tz) { " DST" } else { "" };
                    let offset = format_utc_offset(tz);
                    items.push(CompletionItem {
                        label: display.to_string(),
                        icon_path: Some("__none__".to_string()),
                        score: 90,
                        description: Some(format!("{time_str} {abbr}{dst_tag} · {offset}")),
                        reason: None,
                    });
                }
            }
            return items;
        }

        // Match cities
        for &(key, display, iana) in CITY_MAP {
            if (key.contains(&lower) || lower.contains(key))
                && let Ok(tz) = iana.parse::<Tz>()
            {
                let now = Utc::now().with_timezone(&tz);
                let time_str = format_time_12h(&now);
                let abbr = tz_abbrev(tz);
                let dst_tag = if is_dst(tz) { " DST" } else { "" };
                let offset = format_utc_offset(tz);
                let score = if key.starts_with(&lower) { 100 } else { 70 };
                items.push(CompletionItem {
                    label: display.to_string(),
                    icon_path: Some("__none__".to_string()),
                    score,
                    description: Some(format!("{time_str} {abbr}{dst_tag} · {offset}")),
                    reason: None,
                });
            }
        }

        // Match abbreviations
        for &(abbr_key, display, iana) in TZ_ABBREVS {
            if (abbr_key.contains(&lower) || lower.contains(abbr_key))
                && let Ok(tz) = iana.parse::<Tz>()
            {
                let now = Utc::now().with_timezone(&tz);
                let time_str = format_time_12h(&now);
                let dst_tag = if is_dst(tz) { " DST" } else { "" };
                let offset = format_utc_offset(tz);
                let score = if abbr_key == lower { 100 } else { 60 };
                items.push(CompletionItem {
                    label: display.to_string(),
                    icon_path: Some("__none__".to_string()),
                    score,
                    description: Some(format!("{time_str}{dst_tag} · {offset}")),
                    reason: None,
                });
            }
        }

        // Deduplicate by label
        items.sort_by(|a, b| b.score.cmp(&a.score));
        items.dedup_by(|a, b| a.label == b.label);
        items.truncate(20);
        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_city() {
        let (name, tz) = resolve_tz("tokyo").unwrap();
        assert_eq!(name, "Tokyo");
        assert_eq!(tz, "Asia/Tokyo".parse::<Tz>().unwrap());

        let (name, _) = resolve_tz("new york").unwrap();
        assert_eq!(name, "New York");

        let (name, _) = resolve_tz("ny").unwrap();
        assert_eq!(name, "New York");

        let (name, _) = resolve_tz("sf").unwrap();
        assert_eq!(name, "San Francisco");
    }

    #[test]
    fn resolve_abbreviation() {
        let (name, tz) = resolve_tz("est").unwrap();
        assert_eq!(name, "EST");
        assert_eq!(tz, "America/New_York".parse::<Tz>().unwrap());

        let (name, _) = resolve_tz("ist").unwrap();
        assert_eq!(name, "IST");

        let (name, _) = resolve_tz("utc").unwrap();
        assert_eq!(name, "UTC");
    }

    #[test]
    fn resolve_iana() {
        let (_, tz) = resolve_tz("America/Chicago").unwrap();
        assert_eq!(tz, "America/Chicago".parse::<Tz>().unwrap());
    }

    #[test]
    fn resolve_informal_abbreviations() {
        let (name, _) = resolve_tz("ct").unwrap();
        assert_eq!(name, "CT");
        let (name, _) = resolve_tz("et").unwrap();
        assert_eq!(name, "ET");
        let (name, _) = resolve_tz("pt").unwrap();
        assert_eq!(name, "PT");
        let (name, _) = resolve_tz("mt").unwrap();
        assert_eq!(name, "MT");
    }

    #[test]
    fn resolve_unknown() {
        assert!(resolve_tz("narnia").is_none());
        assert!(resolve_tz("").is_none());
    }

    #[test]
    fn parse_time_12h() {
        assert_eq!(
            parse_time("3pm"),
            Some(NaiveTime::from_hms_opt(15, 0, 0).unwrap())
        );
        assert_eq!(
            parse_time("3am"),
            Some(NaiveTime::from_hms_opt(3, 0, 0).unwrap())
        );
        assert_eq!(
            parse_time("12pm"),
            Some(NaiveTime::from_hms_opt(12, 0, 0).unwrap())
        );
        assert_eq!(
            parse_time("12am"),
            Some(NaiveTime::from_hms_opt(0, 0, 0).unwrap())
        );
        assert_eq!(
            parse_time("3:30pm"),
            Some(NaiveTime::from_hms_opt(15, 30, 0).unwrap())
        );
        assert_eq!(
            parse_time("11:45am"),
            Some(NaiveTime::from_hms_opt(11, 45, 0).unwrap())
        );
    }

    #[test]
    fn parse_time_24h() {
        assert_eq!(
            parse_time("14:00"),
            Some(NaiveTime::from_hms_opt(14, 0, 0).unwrap())
        );
        assert_eq!(
            parse_time("0:00"),
            Some(NaiveTime::from_hms_opt(0, 0, 0).unwrap())
        );
        assert_eq!(
            parse_time("23:59"),
            Some(NaiveTime::from_hms_opt(23, 59, 0).unwrap())
        );
    }

    #[test]
    fn parse_time_named() {
        assert_eq!(
            parse_time("noon"),
            Some(NaiveTime::from_hms_opt(12, 0, 0).unwrap())
        );
        assert_eq!(
            parse_time("midnight"),
            Some(NaiveTime::from_hms_opt(0, 0, 0).unwrap())
        );
        // "now" returns current time — just verify it parses
        assert!(parse_time("now").is_some());
    }

    #[test]
    fn parse_time_invalid() {
        assert!(parse_time("25:00").is_none());
        assert!(parse_time("abc").is_none());
        assert!(parse_time("13pm").is_none());
        assert!(parse_time("").is_none());
    }

    #[test]
    fn world_clock_works() {
        let result = world_clock("tokyo").unwrap();
        assert!(result.contains("Tokyo"));
        assert!(result.contains("JST"));
        assert!(result.contains("UTC+9"));

        let result = world_clock("london").unwrap();
        assert!(result.contains("London"));

        let result = world_clock("est").unwrap();
        assert!(result.contains("EST"));
    }

    #[test]
    fn world_clock_unknown() {
        assert!(world_clock("narnia").is_err());
    }

    #[test]
    fn convert_time_works() {
        let result = convert_time("3pm EST to IST").unwrap();
        assert!(result.contains("EST"));
        assert!(result.contains("IST"));
        // 3pm EST = 1:30 AM IST (+1 day) — verify structure
        assert!(result.contains("→"));

        let result = convert_time("noon UTC to JST").unwrap();
        assert!(result.contains("UTC"));
        assert!(result.contains("JST"));
    }

    #[test]
    fn convert_time_invalid() {
        assert!(convert_time("3pm to IST").is_err()); // no source tz
        assert!(convert_time("hello to goodbye").is_err()); // nonsense
    }

    #[test]
    fn is_tz_conversion_detection() {
        assert!(is_tz_conversion("3pm EST to IST"));
        assert!(is_tz_conversion("noon UTC to PST"));
        assert!(is_tz_conversion("now CST to JST"));
        assert!(is_tz_conversion("14:00 GMT to EST"));

        // Informal abbreviations
        assert!(is_tz_conversion("2:30pm IST to CT"));
        assert!(is_tz_conversion("3pm ET to PT"));

        // Not timezone conversions
        assert!(!is_tz_conversion("tokyo")); // no " to "
        assert!(!is_tz_conversion("2 hours to minutes")); // units, not timezones
        assert!(!is_tz_conversion("hello to world")); // nonsense
    }

    #[test]
    fn format_utc_offset_works() {
        let ist = "Asia/Kolkata".parse::<Tz>().unwrap();
        assert_eq!(format_utc_offset(ist), "UTC+5:30");

        let utc = "Etc/UTC".parse::<Tz>().unwrap();
        assert_eq!(format_utc_offset(utc), "UTC+0");
    }

    #[test]
    fn completions_empty_shows_popular() {
        let handler = TimeHandler::new();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let items = rt.block_on(handler.completions(""));
        assert!(!items.is_empty());
        assert!(items.len() <= 20);
        // Should contain popular cities
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.iter().any(|l| l.contains("New York")));
        assert!(labels.iter().any(|l| l.contains("London")));
        assert!(labels.iter().any(|l| l.contains("Tokyo")));
    }

    #[test]
    fn completions_filter() {
        let handler = TimeHandler::new();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let items = rt.block_on(handler.completions("tok"));
        assert!(!items.is_empty());
        assert!(items.iter().any(|i| i.label.contains("Tokyo")));
    }
}
