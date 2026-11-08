//! A dashboard's time zone: every time a query returns with its offset is
//! shown in it, so two dashboards — or two columns written by different
//! scripts, one in UTC and one in Hong Kong time — read on one clock.
//!
//! ```text
//! dashboard "Operations" {
//!   timezone = "Asia/Hong_Kong"   // or "UTC", "local", "+08:00"
//! }
//! ```
//!
//! Only a time that says its offset (ending in `Z`, `+08:00` or `+0800`) can
//! be moved to another zone. One that does not is shown as written: guessing
//! its zone is how two clocks get mixed up.

use chrono::{DateTime, FixedOffset, Local, NaiveDateTime, Offset, TimeZone, Utc};

/// A zone a dashboard shows its times in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Zone {
    Utc,
    /// The zone of the computer showing the dashboard.
    Local,
    Fixed(FixedOffset),
    Named(chrono_tz::Tz),
}

impl Zone {
    /// `UTC`, `local`, an IANA name (`Asia/Hong_Kong`) or an offset
    /// (`+08:00`, `-0500`).
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        match text.to_ascii_lowercase().as_str() {
            "utc" | "z" | "gmt" => return Some(Self::Utc),
            "local" => return Some(Self::Local),
            _ => {}
        }
        if let Some(offset) = parse_offset(text) {
            return Some(Self::Fixed(offset));
        }
        text.parse::<chrono_tz::Tz>().ok().map(Self::Named)
    }

    /// `time` moved to this zone, as `YYYY-MM-DD HH:MM:SS` (fractions kept),
    /// or `None` when `time` names no offset and so cannot be moved.
    pub fn convert(&self, time: &str) -> Option<String> {
        let parsed = parse_with_offset(time)?;
        let fraction = parsed.timestamp_subsec_nanos() != 0;
        let format = if fraction { "%Y-%m-%d %H:%M:%S%.3f" } else { "%Y-%m-%d %H:%M:%S" };
        Some(match self {
            Self::Utc => parsed.with_timezone(&Utc).format(format).to_string(),
            Self::Local => parsed.with_timezone(&Local).format(format).to_string(),
            Self::Fixed(offset) => parsed.with_timezone(offset).format(format).to_string(),
            Self::Named(tz) => parsed.with_timezone(tz).format(format).to_string(),
        })
    }

    /// How the zone is named to the person reading: `UTC`, `UTC+08:00`, or
    /// the IANA name with its offset now (`Asia/Hong_Kong, UTC+08:00`).
    pub fn label(&self) -> String {
        let now = Utc::now();
        match self {
            Self::Utc => "UTC".into(),
            Self::Local => format!("local, UTC{}", Local.offset_from_utc_datetime(&now.naive_utc()).fix()),
            Self::Fixed(offset) => format!("UTC{offset}"),
            Self::Named(tz) => format!("{}, UTC{}", tz.name(), tz.offset_from_utc_datetime(&now.naive_utc()).fix()),
        }
    }
}

/// `+08:00`, `+0800`, `-05`.
fn parse_offset(text: &str) -> Option<FixedOffset> {
    let (sign, rest) = match text.as_bytes().first()? {
        b'+' => (1, &text[1..]),
        b'-' => (-1, &text[1..]),
        _ => return None,
    };
    let digits: String = rest.chars().filter(|c| *c != ':').collect();
    if !digits.chars().all(|c| c.is_ascii_digit()) || !matches!(digits.len(), 2 | 4) {
        return None;
    }
    let hours: i32 = digits[..2].parse().ok()?;
    let minutes: i32 = if digits.len() == 4 { digits[2..].parse().ok()? } else { 0 };
    if hours > 14 || minutes > 59 {
        return None;
    }
    FixedOffset::east_opt(sign * (hours * 3600 + minutes * 60))
}

/// A time that names its offset: RFC 3339, with a space for the `T`, a
/// `+0800` offset or a trailing `UTC`.
fn parse_with_offset(text: &str) -> Option<DateTime<FixedOffset>> {
    let text = text.trim();
    if let Ok(time) = DateTime::parse_from_rfc3339(text) {
        return Some(time);
    }
    let spaced = text.replacen(' ', "T", 1);
    if let Ok(time) = DateTime::parse_from_rfc3339(&spaced) {
        return Some(time);
    }
    for format in ["%Y-%m-%dT%H:%M:%S%.f%z", "%Y-%m-%dT%H:%M%z"] {
        if let Ok(time) = DateTime::parse_from_str(&spaced, format) {
            return Some(time);
        }
    }
    let bare = spaced.strip_suffix(" UTC").or_else(|| spaced.strip_suffix("UTC"))?.trim_end();
    ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M"]
        .iter()
        .find_map(|format| NaiveDateTime::parse_from_str(bare, format).ok())
        .map(|naive| Utc.from_utc_datetime(&naive).fixed_offset())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zones_by_name_offset_or_word() {
        assert_eq!(Zone::parse("UTC"), Some(Zone::Utc));
        assert_eq!(Zone::parse("local"), Some(Zone::Local));
        assert_eq!(Zone::parse("+08:00"), Some(Zone::Fixed(FixedOffset::east_opt(8 * 3600).unwrap())));
        assert_eq!(Zone::parse("-0530"), Some(Zone::Fixed(FixedOffset::west_opt(5 * 3600 + 1800).unwrap())));
        assert_eq!(Zone::parse("Asia/Hong_Kong"), Some(Zone::Named(chrono_tz::Asia::Hong_Kong)));
        assert_eq!(Zone::parse("HKT"), None, "an abbreviation names no one zone");
        assert_eq!(Zone::parse("+25:00"), None);
    }

    #[test]
    fn a_time_with_its_offset_moves_and_one_without_stays() {
        let hk = Zone::parse("Asia/Hong_Kong").unwrap();
        assert_eq!(hk.convert("1970-01-01T02:15:00Z").as_deref(), Some("1970-01-01 10:15:00"));
        assert_eq!(hk.convert("1970-01-01 02:15:00+00:00").as_deref(), Some("1970-01-01 10:15:00"));
        assert_eq!(hk.convert("1970-01-01T02:15:00.250Z").as_deref(), Some("1970-01-01 10:15:00.250"));
        assert_eq!(hk.convert("1970-01-01 02:15 UTC").as_deref(), Some("1970-01-01 10:15:00"));
        assert_eq!(Zone::Utc.convert("1970-01-01T10:15:00+0800").as_deref(), Some("1970-01-01 02:15:00"));
        assert_eq!(hk.convert("1970-01-01 10:15"), None);
        assert_eq!(hk.convert("1970-01-01"), None);
    }
}
