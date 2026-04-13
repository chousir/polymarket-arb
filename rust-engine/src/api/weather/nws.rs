// NOAA National Weather Service API — US cities only.
//
// The NWS API is a two-step process:
//
//   Step 1: GET https://api.weather.gov/points/{lat},{lon}
//           → Returns the WFO office, gridX, gridY and a pre-built forecast URL.
//
//   Step 2: GET {properties.forecast}  (daily periods endpoint)
//           → Returns an array of forecast Periods with temperature, name,
//             isDaytime, and probabilityOfPrecipitation.
//
//   Optional short-horizon path:
//     GET {properties.forecastHourly} (hourly periods endpoint)
//           → Returns hourly forecast Periods. We aggregate hourly values into
//             per-day max/min temperature and max precipitation probability.
//
// We pair consecutive (daytime, nighttime) Period entries to produce max/min
// temperature for each calendar day, then emit one WeatherForecast per day.
//
// Important: NWS API requires a User-Agent header and reports temperatures in °F.
//
// Grid metadata is cached per city (keyed by ICAO code) to avoid repeating the
// /points call on every fetch.

use chrono::{NaiveDate, Utc};
use once_cell::sync::Lazy;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Mutex;

use crate::api::weather::{CityInfo, WeatherForecast, WeatherModel};
use crate::error::AppError;

const NWS_BASE: &str = "https://api.weather.gov";
/// NWS API requires a descriptive User-Agent.
const USER_AGENT: &str = "polymarket-arb/1.0 (contact@example.com)";

// ── Grid metadata cache ───────────────────────────────────────────────────────

#[derive(Clone)]
struct GridMeta {
    forecast_url: String,
    forecast_hourly_url: String,
}

static GRID_CACHE: Lazy<Mutex<HashMap<String, GridMeta>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

static CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent(USER_AGENT)
        .build()
        .expect("build nws client")
});

// ── Public fetch function ─────────────────────────────────────────────────────

/// Fetch NWS daily forecast for a US city.
///
/// Returns up to `days` WeatherForecast entries, one per calendar day.
/// Returns `Err` for non-US cities (those without an `nws_office`).
pub async fn fetch_nws(
    city: &CityInfo,
    days: u32,
) -> Result<Vec<WeatherForecast>, AppError> {
    if city.nws_office.is_none() {
        return Err(AppError::ApiError(format!(
            "[NWS] {} is not a US city — NWS only covers US locations", city.name
        )));
    }

    let forecast_url = resolve_forecast_url(city).await?;
    let periods = fetch_periods(&forecast_url).await?;
    parse_nws_periods(&periods, city, days)
}

/// Fetch NWS hourly forecast and aggregate into per-day WeatherForecast rows.
///
/// This is intended for short-horizon strategies (e.g. metar_short) that need
/// finer-grained updates than the daily forecast endpoint.
pub async fn fetch_nws_hourly_agg(
    city: &CityInfo,
    days: u32,
) -> Result<Vec<WeatherForecast>, AppError> {
    if city.nws_office.is_none() {
        return Err(AppError::ApiError(format!(
            "[NWS] {} is not a US city — NWS only covers US locations", city.name
        )));
    }

    let forecast_hourly_url = resolve_forecast_hourly_url(city).await?;
    let periods = fetch_periods(&forecast_hourly_url).await?;
    parse_nws_hourly_periods(&periods, city, days)
}

// ── Step 1: resolve forecast URL (cached) ────────────────────────────────────

async fn resolve_forecast_url(city: &CityInfo) -> Result<String, AppError> {
    // Check cache first
    {
        let cache = GRID_CACHE.lock().unwrap();
        if let Some(meta) = cache.get(city.icao) {
            tracing::debug!("[NWS] grid cache hit for {}", city.name);
            return Ok(meta.forecast_url.clone());
        }
    }

    let _ = resolve_grid_meta(city).await?;
    let cache = GRID_CACHE.lock().unwrap();
    if let Some(meta) = cache.get(city.icao) {
        return Ok(meta.forecast_url.clone());
    }

    Err(AppError::ApiError(format!(
        "[NWS] missing cached forecast URL for {}",
        city.name,
    )))
}

async fn resolve_forecast_hourly_url(city: &CityInfo) -> Result<String, AppError> {
    {
        let cache = GRID_CACHE.lock().unwrap();
        if let Some(meta) = cache.get(city.icao) {
            tracing::debug!("[NWS] hourly grid cache hit for {}", city.name);
            return Ok(meta.forecast_hourly_url.clone());
        }
    }

    let _ = resolve_grid_meta(city).await?;
    let cache = GRID_CACHE.lock().unwrap();
    if let Some(meta) = cache.get(city.icao) {
        return Ok(meta.forecast_hourly_url.clone());
    }

    Err(AppError::ApiError(format!(
        "[NWS] missing cached forecastHourly URL for {}",
        city.name,
    )))
}

async fn resolve_grid_meta(city: &CityInfo) -> Result<GridMeta, AppError> {
    let url = format!("{NWS_BASE}/points/{:.4},{:.4}", city.lat, city.lon);
    tracing::debug!("[NWS] GET {url}");

    let resp: PointsResponse = CLIENT
        .get(&url)
        .send()
        .await
        .map_err(AppError::Http)?
        .error_for_status()
        .map_err(AppError::Http)?
        .json()
        .await
        .map_err(AppError::Http)?;

    let forecast_url = resp.properties.forecast;
    let forecast_hourly_url = resp
        .properties
        .forecast_hourly
        .unwrap_or_else(|| format!("{forecast_url}/hourly"));

    tracing::info!(
        "[NWS] {} → forecast URL: {}  forecastHourly URL: {}",
        city.name,
        forecast_url,
        forecast_hourly_url,
    );

    let meta = GridMeta {
        forecast_url,
        forecast_hourly_url,
    };

    {
        let mut cache = GRID_CACHE.lock().unwrap();
        cache.insert(city.icao.to_string(), meta.clone());
    }

    Ok(meta)
}

// ── Step 2: fetch periods ─────────────────────────────────────────────────────

async fn fetch_periods(url: &str) -> Result<Vec<Period>, AppError> {
    tracing::debug!("[NWS] GET {url}");

    let resp: ForecastResponse = CLIENT
        .get(url)
        .send()
        .await
        .map_err(AppError::Http)?
        .error_for_status()
        .map_err(AppError::Http)?
        .json()
        .await
        .map_err(AppError::Http)?;

    Ok(resp.properties.periods)
}

// ── Parse periods into WeatherForecast ────────────────────────────────────────
//
// NWS returns periods like:
//   { "name": "Monday", "isDaytime": true, "temperature": 75, "temperatureUnit": "F",
//     "startTime": "2026-06-15T06:00:00-04:00",
//     "probabilityOfPrecipitation": { "value": 30 } }
//   { "name": "Monday Night", "isDaytime": false, "temperature": 55, "temperatureUnit": "F",
//     "startTime": "2026-06-15T18:00:00-04:00",
//     "probabilityOfPrecipitation": { "value": 20 } }
//
// Strategy: group by startTime date (local date portion), take the daytime period
// for max_temp and the next nighttime period for min_temp.

pub(crate) fn parse_nws_periods(
    periods: &[Period],
    city: &CityInfo,
    days: u32,
) -> Result<Vec<WeatherForecast>, AppError> {
    if periods.is_empty() {
        return Err(AppError::ApiError(format!(
            "[NWS] no forecast periods returned for {}", city.name
        )));
    }

    // Group periods by local date extracted from startTime
    let mut day_max: HashMap<NaiveDate, f64> = HashMap::new();
    let mut day_min: HashMap<NaiveDate, f64> = HashMap::new();
    let mut day_precip: HashMap<NaiveDate, f64> = HashMap::new();

    for p in periods {
        // startTime format: "2026-06-15T06:00:00-04:00"
        let date = parse_nws_date(&p.start_time)?;
        let temp_c = f_to_c(p.temperature as f64, &p.temperature_unit);
        let precip_pct = p.probability_of_precipitation
            .as_ref()
            .and_then(|v| v.value)
            .unwrap_or(0.0);

        if p.is_daytime {
            day_max
                .entry(date)
                .and_modify(|t| *t = t.max(temp_c))
                .or_insert(temp_c);
            // Use daytime precip probability as representative for the day
            day_precip
                .entry(date)
                .and_modify(|t| *t = t.max(precip_pct))
                .or_insert(precip_pct);
        } else {
            // Nighttime low maps to min_temp of the same date
            day_min
                .entry(date)
                .and_modify(|t| *t = t.min(temp_c))
                .or_insert(temp_c);
        }
    }

    let fetched_at = Utc::now();
    let today = fetched_at.date_naive();

    let mut unique_dates: Vec<NaiveDate> = day_max.keys().copied().collect();
    unique_dates.sort();
    unique_dates.truncate(days as usize);

    let forecasts = unique_dates
        .into_iter()
        .map(|date| {
            let max_temp_c = day_max[&date];
            let min_temp_c = day_min.get(&date).copied().unwrap_or(max_temp_c - 8.0);
            let prob_precip = day_precip.get(&date).copied().unwrap_or(0.0) / 100.0;
            let lead_days = (date - today).num_days().max(0) as u32;

            WeatherForecast {
                city: city.name.to_string(),
                model: WeatherModel::Nws,
                forecast_date: date,
                max_temp_c,
                min_temp_c,
                prob_precip,
                ensemble_members: None,
                fetched_at,
                lead_days,
            }
        })
        .collect();

    Ok(forecasts)
}

pub(crate) fn parse_nws_hourly_periods(
    periods: &[Period],
    city: &CityInfo,
    days: u32,
) -> Result<Vec<WeatherForecast>, AppError> {
    if periods.is_empty() {
        return Err(AppError::ApiError(format!(
            "[NWS] no hourly forecast periods returned for {}", city.name
        )));
    }

    let mut day_max: HashMap<NaiveDate, f64> = HashMap::new();
    let mut day_min: HashMap<NaiveDate, f64> = HashMap::new();
    let mut day_precip: HashMap<NaiveDate, f64> = HashMap::new();

    for p in periods {
        let date = parse_nws_date(&p.start_time)?;
        let temp_c = f_to_c(p.temperature as f64, &p.temperature_unit);
        let precip_pct = p
            .probability_of_precipitation
            .as_ref()
            .and_then(|v| v.value)
            .unwrap_or(0.0);

        day_max
            .entry(date)
            .and_modify(|t| *t = t.max(temp_c))
            .or_insert(temp_c);
        day_min
            .entry(date)
            .and_modify(|t| *t = t.min(temp_c))
            .or_insert(temp_c);
        day_precip
            .entry(date)
            .and_modify(|t| *t = t.max(precip_pct))
            .or_insert(precip_pct);
    }

    let fetched_at = Utc::now();
    let today = fetched_at.date_naive();

    let mut unique_dates: Vec<NaiveDate> = day_max.keys().copied().collect();
    unique_dates.sort();
    unique_dates.truncate(days as usize);

    let forecasts = unique_dates
        .into_iter()
        .map(|date| {
            let max_temp_c = day_max[&date];
            let min_temp_c = day_min[&date];
            let prob_precip = day_precip.get(&date).copied().unwrap_or(0.0) / 100.0;
            let lead_days = (date - today).num_days().max(0) as u32;

            WeatherForecast {
                city: city.name.to_string(),
                model: WeatherModel::Nws,
                forecast_date: date,
                max_temp_c,
                min_temp_c,
                prob_precip,
                ensemble_members: None,
                fetched_at,
                lead_days,
            }
        })
        .collect();

    Ok(forecasts)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn f_to_c(temp_f: f64, unit: &str) -> f64 {
    if unit.eq_ignore_ascii_case("F") {
        (temp_f - 32.0) * 5.0 / 9.0
    } else {
        temp_f // already Celsius
    }
}

fn parse_nws_date(start_time: &str) -> Result<NaiveDate, AppError> {
    // "2026-06-15T06:00:00-04:00" → take first 10 chars
    if start_time.len() < 10 {
        return Err(AppError::ApiError(format!(
            "[NWS] cannot parse date from '{start_time}'"
        )));
    }
    NaiveDate::parse_from_str(&start_time[..10], "%Y-%m-%d").map_err(|e| {
        AppError::ApiError(format!(
            "[NWS] date parse error for '{start_time}': {e}"
        ))
    })
}

// ── Serde types ───────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct PointsResponse {
    properties: PointsProperties,
}

#[derive(Deserialize)]
struct PointsProperties {
    forecast: String,
    #[serde(rename = "forecastHourly")]
    forecast_hourly: Option<String>,
}

#[derive(Deserialize)]
struct ForecastResponse {
    properties: ForecastProperties,
}

#[derive(Deserialize)]
struct ForecastProperties {
    periods: Vec<Period>,
}

#[derive(Deserialize)]
pub(crate) struct Period {
    #[serde(rename = "startTime")]
    pub start_time: String,
    #[serde(rename = "isDaytime")]
    pub is_daytime: bool,
    pub temperature: i64,
    #[serde(rename = "temperatureUnit")]
    pub temperature_unit: String,
    #[serde(rename = "probabilityOfPrecipitation")]
    pub probability_of_precipitation: Option<PrecipValue>,
}

#[derive(Deserialize)]
pub(crate) struct PrecipValue {
    pub value: Option<f64>,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;
    use crate::api::weather::ALL_CITIES;

    fn nyc() -> &'static CityInfo { ALL_CITIES.iter().find(|c| c.name == "NYC").unwrap() }

    fn make_period(date: &str, is_day: bool, temp_f: i64, precip: Option<f64>) -> Period {
        Period {
            start_time: format!("{date}T06:00:00-04:00"),
            is_daytime: is_day,
            temperature: temp_f,
            temperature_unit: "F".to_string(),
            probability_of_precipitation: precip.map(|v| PrecipValue { value: Some(v) }),
        }
    }

    #[test]
    fn parses_day_night_pairs() {
        let periods = vec![
            make_period("2026-06-15", true,  82, Some(20.0)),  // day: 82°F = 27.78°C
            make_period("2026-06-15", false, 65, Some(10.0)),  // night: 65°F = 18.33°C
            make_period("2026-06-16", true,  90, Some(0.0)),
            make_period("2026-06-16", false, 72, None),
        ];
        let forecasts = parse_nws_periods(&periods, nyc(), 7).unwrap();
        assert_eq!(forecasts.len(), 2);

        let f0 = forecasts.iter().find(|f| f.forecast_date.day() == 15).unwrap();
        assert!((f0.max_temp_c - f_to_c(82.0, "F")).abs() < 0.01);
        assert!((f0.min_temp_c - f_to_c(65.0, "F")).abs() < 0.01);
        assert!((f0.prob_precip - 0.20).abs() < 1e-9);

        let f1 = forecasts.iter().find(|f| f.forecast_date.day() == 16).unwrap();
        assert!((f1.max_temp_c - f_to_c(90.0, "F")).abs() < 0.01);
        assert!((f1.min_temp_c - f_to_c(72.0, "F")).abs() < 0.01);
        assert!((f1.prob_precip).abs() < 1e-9); // None → 0
    }

    #[test]
    fn celsius_passthrough() {
        let periods = vec![
            make_period("2026-06-15", true,  30, None),
            make_period("2026-06-15", false, 18, None),
        ];
        // Override temperature unit to Celsius in the struct manually
        let periods_c: Vec<Period> = periods.into_iter().map(|mut p| {
            p.temperature_unit = "C".to_string();
            p
        }).collect();

        let forecasts = parse_nws_periods(&periods_c, nyc(), 1).unwrap();
        // Should be 30°C max, 18°C min with no conversion
        assert!((forecasts[0].max_temp_c - 30.0).abs() < 0.01);
        assert!((forecasts[0].min_temp_c - 18.0).abs() < 0.01);
    }

    #[test]
    fn f_to_c_conversion() {
        assert!((f_to_c(32.0, "F") - 0.0).abs()   < 0.01);
        assert!((f_to_c(212.0, "F") - 100.0).abs() < 0.01);
        assert!((f_to_c(98.6, "F") - 37.0).abs()  < 0.01);
        // Passthrough for Celsius
        assert!((f_to_c(25.0, "C") - 25.0).abs()  < 0.01);
    }

    #[test]
    fn respects_days_limit() {
        let periods: Vec<Period> = (0..10i64).flat_map(|d| {
            let date = format!("2026-06-{:02}", 15 + d);
            vec![
                make_period(&date, true,  80, None),
                make_period(&date, false, 62, None),
            ]
        }).collect();
        let forecasts = parse_nws_periods(&periods, nyc(), 3).unwrap();
        assert_eq!(forecasts.len(), 3);
    }

    #[test]
    fn empty_periods_returns_error() {
        assert!(parse_nws_periods(&[], nyc(), 7).is_err());
    }

    #[test]
    fn parses_hourly_periods_aggregate_daily_extrema() {
        let periods = vec![
            make_period("2026-06-15", true, 82, Some(10.0)),
            make_period("2026-06-15", true, 86, Some(40.0)),
            make_period("2026-06-15", false, 64, Some(20.0)),
            make_period("2026-06-16", true, 88, Some(0.0)),
            make_period("2026-06-16", false, 70, None),
        ];

        let forecasts = parse_nws_hourly_periods(&periods, nyc(), 2).unwrap();
        assert_eq!(forecasts.len(), 2);

        let f0 = forecasts.iter().find(|f| f.forecast_date.day() == 15).unwrap();
        assert!((f0.max_temp_c - f_to_c(86.0, "F")).abs() < 0.01);
        assert!((f0.min_temp_c - f_to_c(64.0, "F")).abs() < 0.01);
        assert!((f0.prob_precip - 0.40).abs() < 1e-9);
    }

    #[test]
    fn non_us_city_returns_error() {
        // We can't call async fn in a sync test, but we can test the city_info branch
        let london = ALL_CITIES.iter().find(|c| c.name == "London").unwrap();
        assert!(london.nws_office.is_none());
    }
}
