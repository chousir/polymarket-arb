// METAR real-time aviation weather observation.
//
// Endpoint: GET https://aviationweather.gov/api/data/metar
//               ?ids={ICAO}&format=json&hours={hours}
//
// METAR returns current surface observations (temperature, dewpoint, wind, etc.)
// This is NOT a forecast — it tells us the current ground truth, which is useful
// for two things:
//   1. Short-term (< 24h) market validation: if METAR already shows 28°C at 10am,
//      a "max temp > 30°C today" market probability should be re-evaluated.
//   2. Trend input for weather_decision: pairing METAR observations with NWS/GFS
//      forecasts provides a recent-observations anchor.
//
// The returned WeatherForecast has:
//   lead_days = 0  (this is an observation, not a forecast)
//   max_temp_c = min_temp_c = current temperature
//   prob_precip = 0.0  (METAR doesn't carry precip probability)
//   ensemble_members = None

use chrono::{NaiveDate, Utc};
use once_cell::sync::Lazy;
use serde::Deserialize;

use crate::api::weather::{CityInfo, WeatherForecast, WeatherModel};
use crate::error::AppError;

const METAR_BASE: &str = "https://aviationweather.gov/api/data/metar";

static CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("polymarket-arb/1.0 (weather strategy)")
        .build()
        .expect("build metar client")
});

// ── Public fetch function ─────────────────────────────────────────────────────

/// Fetch the most recent METAR observation for `city`.
///
/// `hours` controls how far back to search (1–3 is usually sufficient).
/// Returns the single most recent observation as a `WeatherForecast` with
/// `lead_days = 0` and `model = WeatherModel::Metar`.
pub async fn fetch_metar(
    city: &CityInfo,
    hours: u32,
) -> Result<WeatherForecast, AppError> {
    let url = format!(
        "{METAR_BASE}?ids={}&format=json&hours={hours}",
        city.icao
    );
    tracing::debug!("[METAR] GET {url}");

    let text = CLIENT
        .get(&url)
        .send()
        .await
        .map_err(AppError::Http)?
        .error_for_status()
        .map_err(AppError::Http)?
        .text()
        .await
        .map_err(AppError::Http)?;

    let records: Vec<MetarRecord> =
        serde_json::from_str(&text).map_err(AppError::Json)?;

    // Most recent first — aviationweather.gov returns newest at index 0
    let record = records.into_iter().next().ok_or_else(|| {
        AppError::ApiError(format!(
            "[METAR] no observations found for {} (ICAO: {})", city.name, city.icao
        ))
    })?;

    parse_metar_record(&record, city)
}

// ── Parse ─────────────────────────────────────────────────────────────────────

pub(crate) fn parse_metar_record(
    record: &MetarRecord,
    city: &CityInfo,
) -> Result<WeatherForecast, AppError> {
    let temp_c = record.temp.ok_or_else(|| {
        AppError::ApiError(format!(
            "[METAR] {} observation has no temperature field", city.name
        ))
    })?;

    // reportTime format: "2026-04-13 12:00:00" or ISO-8601 variant
    let forecast_date = parse_metar_date(&record.report_time);

    let fetched_at = Utc::now();

    Ok(WeatherForecast {
        city: city.name.to_string(),
        model: WeatherModel::Metar,
        forecast_date: forecast_date.unwrap_or_else(|| fetched_at.date_naive()),
        // METAR is current observation — max and min both equal current temp
        max_temp_c: temp_c,
        min_temp_c: temp_c,
        prob_precip: 0.0,
        ensemble_members: None,
        fetched_at,
        lead_days: 0,
    })
}

fn parse_metar_date(s: &str) -> Option<NaiveDate> {
    // Try "YYYY-MM-DD HH:MM:SS" first
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        return Some(dt.date());
    }
    // Try ISO-8601 with T separator
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%SZ") {
        return Some(dt.date());
    }
    // Fallback: take first 10 characters
    if s.len() >= 10 {
        return NaiveDate::parse_from_str(&s[..10], "%Y-%m-%d").ok();
    }
    None
}

// ── Serde types ───────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(crate) struct MetarRecord {
    /// ICAO identifier of the reporting station
    #[serde(rename = "icaoId")]
    pub icao_id: String,
    /// Observation time ("2026-04-13 12:00:00" or ISO format)
    #[serde(rename = "reportTime")]
    pub report_time: String,
    /// Current temperature (°C) — may be absent for some stations
    pub temp: Option<f64>,
    /// Dewpoint (°C) — not used by decision layer but logged for debugging
    pub dewp: Option<f64>,
    /// Wind speed (knots)
    pub wspd: Option<f64>,
    /// Visibility (statute miles, or "10+" for >= 10 SM)
    pub visib: Option<serde_json::Value>,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;
    use crate::api::weather::ALL_CITIES;

    fn nyc() -> &'static CityInfo { ALL_CITIES.iter().find(|c| c.name == "NYC").unwrap() }
    fn london() -> &'static CityInfo { ALL_CITIES.iter().find(|c| c.name == "London").unwrap() }

    fn make_record(temp: Option<f64>, report_time: &str) -> MetarRecord {
        MetarRecord {
            icao_id: "KJFK".to_string(),
            report_time: report_time.to_string(),
            temp,
            dewp: Some(10.0),
            wspd: Some(12.0),
            visib: Some(serde_json::Value::String("10+".into())),
        }
    }

    #[test]
    fn parses_temp_and_date() {
        let record = make_record(Some(18.5), "2026-06-15 12:00:00");
        let fc = parse_metar_record(&record, nyc()).unwrap();

        assert_eq!(fc.city, "NYC");
        assert_eq!(fc.model, WeatherModel::Metar);
        assert_eq!(fc.lead_days, 0);
        assert!((fc.max_temp_c - 18.5).abs() < 1e-9);
        assert!((fc.min_temp_c - 18.5).abs() < 1e-9);
        assert!((fc.prob_precip).abs() < 1e-9);
        assert!(fc.ensemble_members.is_none());
        assert_eq!(fc.forecast_date.day(), 15);
        assert_eq!(fc.forecast_date.month(), 6);
    }

    #[test]
    fn iso_timestamp_parsed() {
        let record = make_record(Some(22.0), "2026-07-04T18:30:00Z");
        let fc = parse_metar_record(&record, london()).unwrap();
        assert_eq!(fc.forecast_date.day(), 4);
        assert_eq!(fc.forecast_date.month(), 7);
    }

    #[test]
    fn missing_temp_returns_error() {
        let record = make_record(None, "2026-06-15 12:00:00");
        assert!(parse_metar_record(&record, nyc()).is_err());
    }

    #[test]
    fn negative_temp_handles_correctly() {
        // Cold NYC day in January
        let record = make_record(Some(-8.3), "2026-01-15T06:00:00Z");
        let fc = parse_metar_record(&record, nyc()).unwrap();
        assert!((fc.max_temp_c - (-8.3)).abs() < 1e-9);
    }

    #[test]
    fn malformed_date_falls_back_to_today() {
        let record = make_record(Some(20.0), "not-a-date");
        // Should not error — falls back to today's date
        let fc = parse_metar_record(&record, nyc()).unwrap();
        let today = Utc::now().date_naive();
        assert_eq!(fc.forecast_date, today);
    }

    #[test]
    fn parse_metar_date_formats() {
        assert!(parse_metar_date("2026-06-15 12:00:00").is_some());
        assert!(parse_metar_date("2026-06-15T18:30:00Z").is_some());
        assert!(parse_metar_date("2026-06-15").is_some());
        assert_eq!(parse_metar_date("bad").unwrap_or(NaiveDate::MIN), NaiveDate::MIN);
    }
}
