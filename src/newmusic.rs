use crate::{api::Api, config::Config};
use anyhow::Result;
use chrono::{DateTime, Datelike, Duration, NaiveDate, Timelike, Utc};
use chrono_tz::America::Los_Angeles;
use std::path::Path;

pub fn release_day(now: DateTime<Utc>) -> NaiveDate {
    let local = now.with_timezone(&Los_Angeles);
    let date = local.date_naive();
    let days_since_friday = (local.weekday().num_days_from_monday() + 3) % 7;
    if local.weekday() == chrono::Weekday::Thu && local.hour() >= 21 {
        date + Duration::days(1)
    } else {
        date - Duration::days(days_since_friday.into())
    }
}
pub fn playlist_name(day: NaiveDate) -> String {
    format!("New Music, week of {}", day.format("%b %-d"))
}
pub async fn ensure(api: &mut Api, config: &mut Config, dir: &Path) -> Result<String> {
    let day = release_day(Utc::now());
    let key = day.to_string();
    if let Some(id) = config.weeks.get(&key) {
        return Ok(id.clone());
    }
    let name = playlist_name(day);
    let marker = format!("spotify-cli release week {key}");
    let existing = api
        .playlists()
        .await?
        .into_iter()
        .find(|p| p.name == name && p.description == marker);
    let playlist = match existing {
        Some(p) => p,
        None => api.create_playlist(&name, &marker).await?,
    };
    config.weeks.insert(key, playlist.id.clone());
    config.save(dir)?;
    Ok(playlist.id)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rollover_and_dst() {
        for (time, expected) in [
            ("2026-10-16T03:59:59Z", "2026-10-09"),
            ("2026-10-16T04:00:00Z", "2026-10-16"),
            ("2026-11-06T04:59:59Z", "2026-10-30"),
            ("2026-11-06T05:00:00Z", "2026-11-06"),
            ("2027-01-01T05:00:00Z", "2027-01-01"),
            ("2026-03-09T01:00:00Z", "2026-03-06"),
        ] {
            assert_eq!(release_day(time.parse().unwrap()).to_string(), expected);
        }
        assert_eq!(
            playlist_name(NaiveDate::from_ymd_opt(2026, 10, 16).unwrap()),
            "New Music, week of Oct 16"
        );
    }
}
