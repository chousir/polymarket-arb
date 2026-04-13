// Open-Meteo weather API — GFS, ECMWF IFS, and GFS Ensemble.
//
// All three models are free to use with no API key required.
//
// Standard forecast endpoint (GFS / ECMWF):
//   GET https://api.open-meteo.com/v1/forecast
//       ?latitude={lat}&longitude={lon}
//       &daily=temperature_2m_max,temperature_2m_min,precipitation_probability_max
//       &models={model_code}
//       &temperature_unit=celsius&timezone=auto&forecast_days={days}
//
// Ensemble endpoint (GFS members for probabilistic forecasts):
//   GET https://ensemble-api.open-meteo.com/v1/ensemble
//       ?latitude={lat}&longitude={lon}
//       &daily=temperature_2m_max
//       &models=gfs_seamless
//       &temperature_unit=celsius&timezone=auto&forecast_days={days}
//
// The ensemble response returns one row per (member × day) identified by the
// "member" column in `daily`.  We reconstruct per-day member arrays from this.

use chrono::{NaiveDate, Utc};
use once_cell::sync::Lazy;
use serde::Deserialize;
use std::collections::HashMap;

use crate::api::weather::{CityInfo, WeatherForecast, WeatherModel};
use crate::error::AppError;

const FORECAST_BASE: &str = "https://api.open-meteo.com/v1/forecast";
const ENSEMBLE_BASE: &str = "https://ensemble-api.open-meteo.com/v1/ensemble";

static CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        // Open-Meteo requires a User-Agent header
        .user_agent("polymarket-arb/1.0 (weather strategy)")
        .build()
        .expect("build openmeteo client")
});

// ── Public fetch functions ────────────────────────────────────────────────────

/// Fetch GFS deterministic forecast for `city` over the next `days` days.
pub async fn fetch_gfs(
    city: &CityInfo,
    days: u32,
) -> Result<Vec<WeatherForecast>, AppError> {
    fetch_deterministic(city, days, "gfs_seamless", WeatherModel::Gfs).await
}

/// Fetch GFS hourly forecast and aggregate hourly values into daily extrema.
///
/// Intended for short-horizon strategies that need fresher intraday updates.
pub async fn fetch_gfs_hourly_agg(
    city: &CityInfo,
    days: u32,
) -> Result<Vec<WeatherForecast>, AppError> {
    let url = format!(
        "{FORECAST_BASE}\
         ?latitude={:.4}&longitude={:.4}\
         &hourly=temperature_2m,precipitation_probability\
         &models=gfs_seamless\
         &temperature_unit=celsius\
         &timezone=auto\
         &forecast_days={days}",
        city.lat, city.lon
    );

    tracing::debug!("[OpenMeteo] GFS hourly GET {url}");

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

    let raw: HourlyResponse = serde_json::from_str(&text).map_err(AppError::Json)?;
    parse_hourly_agg_response(&raw, city, WeatherModel::Gfs, days)
}

/// Fetch ECMWF IFS deterministic forecast for `city` over the next `days` days.
pub async fn fetch_ecmwf(
    city: &CityInfo,
    days: u32,
) -> Result<Vec<WeatherForecast>, AppError> {
    fetch_deterministic(city, days, "ecmwf_ifs04", WeatherModel::Ecmwf).await
}

/// Fetch GFS ensemble probabilistic forecast for `city`.
///
/// Each returned `WeatherForecast` will have `ensemble_members` populated with
/// the predicted max-temperature values from each GFS ensemble member.
/// Use `WeatherForecast::ensemble_prob_in_range()` to convert to a probability.
pub async fn fetch_ensemble(
    city: &CityInfo,
    days: u32,
) -> Result<Vec<WeatherForecast>, AppError> {
    let url = format!(
        "{ENSEMBLE_BASE}\
         ?latitude={:.4}&longitude={:.4}\
         &daily=temperature_2m_max,temperature_2m_min\
         &models=gfs_seamless\
         &temperature_unit=celsius\
         &timezone=auto\
         &forecast_days={days}",
        city.lat, city.lon
    );

    tracing::debug!("[OpenMeteo] Ensemble GET {url}");

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

    let raw: EnsembleResponse =
        serde_json::from_str(&text).map_err(AppError::Json)?;

    parse_ensemble_response(&raw, city, days)
}

// ── Internal: deterministic (GFS / ECMWF) ────────────────────────────────────

async fn fetch_deterministic(
    city: &CityInfo,
    days: u32,
    model_code: &str,
    model: WeatherModel,
) -> Result<Vec<WeatherForecast>, AppError> {
    let url = format!(
        "{FORECAST_BASE}\
         ?latitude={:.4}&longitude={:.4}\
         &daily=temperature_2m_max,temperature_2m_min,precipitation_probability_max\
         &models={model_code}\
         &temperature_unit=celsius\
         &timezone=auto\
         &forecast_days={days}",
        city.lat, city.lon
    );

    tracing::debug!("[OpenMeteo] {model} GET {url}");

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

    let raw: ForecastResponse =
        serde_json::from_str(&text).map_err(AppError::Json)?;

    parse_forecast_response(&raw, city, model)
}

// ── Parse deterministic response ──────────────────────────────────────────────

pub(crate) fn parse_forecast_response(
    raw: &ForecastResponse,
    city: &CityInfo,
    model: WeatherModel,
) -> Result<Vec<WeatherForecast>, AppError> {
    let times   = &raw.daily.time;
    let max_t   = &raw.daily.temperature_2m_max;
    let min_t   = &raw.daily.temperature_2m_min;
    let precip  = &raw.daily.precipitation_probability_max;

    if times.is_empty() {
        return Err(AppError::ApiError(format!(
            "[OpenMeteo] empty response for {}", city.name
        )));
    }

    let fetched_at = Utc::now();
    let today = fetched_at.date_naive();

    times.iter().enumerate().map(|(i, date_str)| {
        let forecast_date = NaiveDate::parse_from_str(date_str, "%Y-%m-%d")
            .map_err(|e| AppError::ApiError(format!(
                "[OpenMeteo] bad date '{date_str}': {e}"
            )))?;

        let max_temp_c = max_t.get(i).copied().ok_or_else(|| {
            AppError::ApiError(format!("[OpenMeteo] missing max_temp at index {i}"))
        })?;
        let min_temp_c = min_t.get(i).copied().ok_or_else(|| {
            AppError::ApiError(format!("[OpenMeteo] missing min_temp at index {i}"))
        })?;

        // precipitation_probability_max is 0–100 integer; may be absent for some models
        let prob_precip = precip
            .as_ref()
            .and_then(|v| v.get(i).copied())
            .unwrap_or(0.0)
            / 100.0;

        let lead_days = (forecast_date - today).num_days().max(0) as u32;

        Ok(WeatherForecast {
            city: city.name.to_string(),
            model,
            forecast_date,
            max_temp_c,
            min_temp_c,
            prob_precip,
            ensemble_members: None,
            fetched_at,
            lead_days,
        })
    }).collect()
}

// ── Parse ensemble response ───────────────────────────────────────────────────
//
// The Open-Meteo ensemble API returns one row per ensemble member per day.
// The "member" field in `daily` is an array of member-identifiers (same length
// as `time`), where each consecutive run of `forecast_days` rows belongs to one
// member.
//
// Layout example (2 members, 3 days):
//   time   = ["2026-06-01","2026-06-02","2026-06-03",
//              "2026-06-01","2026-06-02","2026-06-03"]
//   member = ["member01","member01","member01","member02","member02","member02"]
//   temperature_2m_max = [25.1, 26.3, 27.0, 24.8, 25.9, 26.5]

pub(crate) fn parse_ensemble_response(
    raw: &EnsembleResponse,
    city: &CityInfo,
    days: u32,
) -> Result<Vec<WeatherForecast>, AppError> {
    let times = &raw.daily.time;
    let max_t  = &raw.daily.temperature_2m_max;

    if times.is_empty() || max_t.is_empty() {
        return Err(AppError::ApiError(format!(
            "[OpenMeteo/Ensemble] empty response for {}", city.name
        )));
    }

    // Group max-temp values by date → collect all member readings for each day
    let mut by_date: HashMap<String, Vec<f64>> = HashMap::new();
    let mut min_by_date: HashMap<String, Vec<f64>> = HashMap::new();

    for (i, date_str) in times.iter().enumerate() {
        if let Some(&max) = max_t.get(i) {
            by_date.entry(date_str.clone()).or_default().push(max);
        }
        if let Some(min_vec) = &raw.daily.temperature_2m_min {
            if let Some(&min) = min_vec.get(i) {
                min_by_date.entry(date_str.clone()).or_default().push(min);
            }
        }
    }

    let fetched_at = Utc::now();
    let today = fetched_at.date_naive();

    // Build one WeatherForecast per unique date
    let mut results: Vec<WeatherForecast> = Vec::new();
    let mut unique_dates: Vec<String> = by_date.keys().cloned().collect();
    unique_dates.sort();
    unique_dates.truncate(days as usize);

    for date_str in &unique_dates {
        let forecast_date = NaiveDate::parse_from_str(date_str, "%Y-%m-%d")
            .map_err(|e| AppError::ApiError(format!(
                "[OpenMeteo/Ensemble] bad date '{date_str}': {e}"
            )))?;

        let member_maxes = by_date[date_str].clone();
        let mean_max = member_maxes.iter().sum::<f64>() / member_maxes.len() as f64;

        let mean_min = min_by_date
            .get(date_str)
            .map(|v| v.iter().sum::<f64>() / v.len() as f64)
            .unwrap_or(mean_max - 8.0); // rough fallback

        let lead_days = (forecast_date - today).num_days().max(0) as u32;

        results.push(WeatherForecast {
            city: city.name.to_string(),
            model: WeatherModel::Ensemble,
            forecast_date,
            max_temp_c: mean_max,
            min_temp_c: mean_min,
            prob_precip: 0.0, // ensemble doesn't provide precip probability
            ensemble_members: Some(member_maxes),
            fetched_at,
            lead_days,
        });
    }

    Ok(results)
}

pub(crate) fn parse_hourly_agg_response(
    raw: &HourlyResponse,
    city: &CityInfo,
    model: WeatherModel,
    days: u32,
) -> Result<Vec<WeatherForecast>, AppError> {
    let times = &raw.hourly.time;
    let temps = &raw.hourly.temperature_2m;
    let precip = &raw.hourly.precipitation_probability;

    if times.is_empty() || temps.is_empty() {
        return Err(AppError::ApiError(format!(
            "[OpenMeteo] empty hourly response for {}",
            city.name
        )));
    }

    let mut day_max: HashMap<NaiveDate, f64> = HashMap::new();
    let mut day_min: HashMap<NaiveDate, f64> = HashMap::new();
    let mut day_precip: HashMap<NaiveDate, f64> = HashMap::new();

    for (i, ts) in times.iter().enumerate() {
        if ts.len() < 10 {
            continue;
        }
        let date = NaiveDate::parse_from_str(&ts[..10], "%Y-%m-%d").map_err(|e| {
            AppError::ApiError(format!("[OpenMeteo] bad hourly timestamp '{ts}': {e}"))
        })?;
        let Some(&temp_c) = temps.get(i) else {
            continue;
        };

        day_max
            .entry(date)
            .and_modify(|t| *t = t.max(temp_c))
            .or_insert(temp_c);
        day_min
            .entry(date)
            .and_modify(|t| *t = t.min(temp_c))
            .or_insert(temp_c);

        let precip_pct = precip.as_ref().and_then(|v| v.get(i).copied()).unwrap_or(0.0);
        day_precip
            .entry(date)
            .and_modify(|p| *p = p.max(precip_pct))
            .or_insert(precip_pct);
    }

    let fetched_at = Utc::now();
    let today = fetched_at.date_naive();

    let mut unique_dates: Vec<NaiveDate> = day_max.keys().copied().collect();
    unique_dates.sort();
    unique_dates.truncate(days as usize);

    let results = unique_dates
        .into_iter()
        .map(|date| WeatherForecast {
            city: city.name.to_string(),
            model,
            forecast_date: date,
            max_temp_c: day_max[&date],
            min_temp_c: day_min[&date],
            prob_precip: day_precip.get(&date).copied().unwrap_or(0.0) / 100.0,
            ensemble_members: None,
            fetched_at,
            lead_days: (date - today).num_days().max(0) as u32,
        })
        .collect();

    Ok(results)
}

// ── Serde types ───────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(crate) struct ForecastResponse {
    pub daily: ForecastDaily,
}

#[derive(Deserialize)]
pub(crate) struct ForecastDaily {
    pub time: Vec<String>,
    pub temperature_2m_max: Vec<f64>,
    pub temperature_2m_min: Vec<f64>,
    /// Some models don't return precipitation probability
    pub precipitation_probability_max: Option<Vec<f64>>,
}

#[derive(Deserialize)]
pub(crate) struct EnsembleResponse {
    pub daily: EnsembleDaily,
}

#[derive(Deserialize)]
pub(crate) struct HourlyResponse {
    pub hourly: HourlyData,
}

#[derive(Deserialize)]
pub(crate) struct HourlyData {
    pub time: Vec<String>,
    pub temperature_2m: Vec<f64>,
    pub precipitation_probability: Option<Vec<f64>>,
}

#[derive(Deserialize)]
pub(crate) struct EnsembleDaily {
    pub time: Vec<String>,
    /// May be absent in some Open-Meteo ensemble responses; tolerated gracefully.
    #[serde(default)]
    pub member: Vec<String>,
    pub temperature_2m_max: Vec<f64>,
    pub temperature_2m_min: Option<Vec<f64>>,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::weather::ALL_CITIES;

    use chrono::Datelike;

    fn nyc() -> &'static CityInfo { ALL_CITIES.iter().find(|c| c.name == "NYC").unwrap() }

    fn make_forecast_response(
        dates: &[&str],
        max_t: &[f64],
        min_t: &[f64],
        precip: Option<&[f64]>,
    ) -> ForecastResponse {
        ForecastResponse {
            daily: ForecastDaily {
                time: dates.iter().map(|s| s.to_string()).collect(),
                temperature_2m_max: max_t.to_vec(),
                temperature_2m_min: min_t.to_vec(),
                precipitation_probability_max: precip.map(|v| v.to_vec()),
            },
        }
    }

    #[test]
    fn parses_deterministic_forecast() {
        let raw = make_forecast_response(
            &["2026-06-15", "2026-06-16", "2026-06-17"],
            &[28.5, 30.1, 27.8],
            &[18.0, 20.2, 17.5],
            Some(&[20.0, 40.0, 60.0]),
        );
        let forecasts = parse_forecast_response(&raw, nyc(), WeatherModel::Gfs).unwrap();
        assert_eq!(forecasts.len(), 3);

        let f0 = &forecasts[0];
        assert_eq!(f0.city, "NYC");
        assert_eq!(f0.model, WeatherModel::Gfs);
        assert!((f0.max_temp_c - 28.5).abs() < 1e-9);
        assert!((f0.min_temp_c - 18.0).abs() < 1e-9);
        assert!((f0.prob_precip - 0.20).abs() < 1e-9);
        assert!(f0.ensemble_members.is_none());

        assert!((forecasts[2].prob_precip - 0.60).abs() < 1e-9);
    }

    #[test]
    fn parses_ecmwf_without_precip() {
        // ECMWF IFS 0.4° sometimes omits precipitation_probability_max
        let raw = make_forecast_response(
            &["2026-06-15"],
            &[22.0],
            &[14.0],
            None,
        );
        let forecasts = parse_forecast_response(&raw, nyc(), WeatherModel::Ecmwf).unwrap();
        assert_eq!(forecasts.len(), 1);
        assert!((forecasts[0].prob_precip - 0.0).abs() < 1e-9);
    }

    #[test]
    fn parses_ensemble_response_groups_by_date() {
        // 3 members × 2 days
        let raw = EnsembleResponse {
            daily: EnsembleDaily {
                time: vec![
                    "2026-06-15".into(), "2026-06-16".into(),
                    "2026-06-15".into(), "2026-06-16".into(),
                    "2026-06-15".into(), "2026-06-16".into(),
                ],
                member: vec![
                    "member01".into(), "member01".into(),
                    "member02".into(), "member02".into(),
                    "member03".into(), "member03".into(),
                ],
                temperature_2m_max: vec![25.0, 26.0, 27.0, 28.0, 23.0, 24.0],
                temperature_2m_min: None,
            },
        };
        let forecasts = parse_ensemble_response(&raw, nyc(), 2).unwrap();
        assert_eq!(forecasts.len(), 2);

        let f0 = forecasts.iter().find(|f| f.forecast_date.day() == 15).unwrap();
        assert_eq!(f0.member_count(), 3);
        // mean of 25, 27, 23 = 25.0
        assert!((f0.max_temp_c - 25.0).abs() < 1e-6);

        // 2 out of 3 members (27, 25) are in [24, 27] — let's verify
        let p = f0.ensemble_prob_in_range(24.0, 27.0).unwrap();
        // members: 25, 27, 23 → 25 ✓, 27 ✓, 23 ✗ → 2/3
        assert!((p - 2.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn empty_response_returns_error() {
        let raw = make_forecast_response(&[], &[], &[], None);
        assert!(parse_forecast_response(&raw, nyc(), WeatherModel::Gfs).is_err());
    }

    #[test]
    fn lead_days_computed_correctly() {
        // Use a date clearly in the future
        let tomorrow = (Utc::now() + chrono::Duration::days(3))
            .date_naive()
            .format("%Y-%m-%d")
            .to_string();
        let raw = make_forecast_response(&[&tomorrow], &[20.0], &[12.0], None);
        let forecasts = parse_forecast_response(&raw, nyc(), WeatherModel::Gfs).unwrap();
        // lead_days should be 3 (±1 for timezone edge cases)
        assert!(forecasts[0].lead_days >= 2 && forecasts[0].lead_days <= 4);
    }
}
