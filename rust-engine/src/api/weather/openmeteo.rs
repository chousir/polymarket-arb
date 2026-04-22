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
use crate::rate_limit::token_bucket::backoff;

const RETRY_ATTEMPTS: u32 = 3;
const RETRY_BASE_MS: u64 = 1_000;

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

    let mut last_err: Option<AppError> = None;
    for attempt in 0..RETRY_ATTEMPTS {
        if attempt > 0 {
            backoff(attempt - 1, RETRY_BASE_MS).await;
        }
        let result = async {
            CLIENT
                .get(&url)
                .send()
                .await
                .map_err(AppError::Http)?
                .error_for_status()
                .map_err(AppError::Http)?
                .text()
                .await
                .map_err(AppError::Http)
        }
        .await;
        match result {
            Ok(text) => {
                let raw: HourlyResponse =
                    serde_json::from_str(&text).map_err(AppError::Json)?;
                return parse_hourly_agg_response(&raw, city, WeatherModel::Gfs, days);
            }
            Err(e) => {
                tracing::warn!(
                    "[OpenMeteo] GFS hourly {} fetch 失敗 ({}/{}): {e}",
                    city.name, attempt + 1, RETRY_ATTEMPTS
                );
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| AppError::ApiError("GFS hourly retry exhausted".to_string())))
}

/// Fetch ECMWF IFS deterministic forecast for `city` over the next `days` days.
pub async fn fetch_ecmwf(
    city: &CityInfo,
    days: u32,
) -> Result<Vec<WeatherForecast>, AppError> {
    // ecmwf_ifs04 (0.4°) was deprecated by Open-Meteo and now returns all-null
    // temperature fields. ecmwf_ifs025 (0.25°) is the current replacement.
    fetch_deterministic(city, days, "ecmwf_ifs025", WeatherModel::Ecmwf).await
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

    let mut last_err: Option<AppError> = None;
    for attempt in 0..RETRY_ATTEMPTS {
        if attempt > 0 {
            backoff(attempt - 1, RETRY_BASE_MS).await;
        }
        let result = async {
            CLIENT
                .get(&url)
                .send()
                .await
                .map_err(AppError::Http)?
                .error_for_status()
                .map_err(AppError::Http)?
                .text()
                .await
                .map_err(AppError::Http)
        }
        .await;
        match result {
            Ok(text) => {
                let raw: EnsembleResponse =
                    serde_json::from_str(&text).map_err(AppError::Json)?;
                return parse_ensemble_response(&raw, city, days);
            }
            Err(e) => {
                tracing::warn!(
                    "[OpenMeteo] Ensemble {} fetch 失敗 ({}/{}): {e}",
                    city.name, attempt + 1, RETRY_ATTEMPTS
                );
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| AppError::ApiError("Ensemble retry exhausted".to_string())))
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

    let mut last_err: Option<AppError> = None;
    for attempt in 0..RETRY_ATTEMPTS {
        if attempt > 0 {
            backoff(attempt - 1, RETRY_BASE_MS).await;
        }
        let result = async {
            CLIENT
                .get(&url)
                .send()
                .await
                .map_err(AppError::Http)?
                .error_for_status()
                .map_err(AppError::Http)?
                .text()
                .await
                .map_err(AppError::Http)
        }
        .await;
        match result {
            Ok(text) => {
                let raw: ForecastResponse =
                    serde_json::from_str(&text).map_err(AppError::Json)?;
                return parse_forecast_response(&raw, city, model);
            }
            Err(e) => {
                tracing::warn!(
                    "[OpenMeteo] {model} {} fetch 失敗 ({}/{}): {e}",
                    city.name, attempt + 1, RETRY_ATTEMPTS
                );
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| AppError::ApiError(format!("{model} retry exhausted"))))
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

    let mut results = Vec::new();
    for (i, date_str) in times.iter().enumerate() {
        let forecast_date = NaiveDate::parse_from_str(date_str, "%Y-%m-%d")
            .map_err(|e| AppError::ApiError(format!(
                "[OpenMeteo] bad date '{date_str}': {e}"
            )))?;

        // ECMWF IFS may return null beyond its forecast horizon; skip those days.
        let max_temp_c = match max_t.get(i).and_then(|v| *v) {
            Some(v) => v,
            None => {
                tracing::debug!("[OpenMeteo] {} date={date_str} max_temp=null, skipping", city.name);
                continue;
            }
        };
        let min_temp_c = match min_t.get(i).and_then(|v| *v) {
            Some(v) => v,
            None => {
                tracing::debug!("[OpenMeteo] {} date={date_str} min_temp=null, skipping", city.name);
                continue;
            }
        };

        // precipitation_probability_max is 0–100 integer; may be absent for some models
        let prob_precip = precip
            .as_ref()
            .and_then(|v| v.get(i).copied())
            .unwrap_or(0.0)
            / 100.0;

        let lead_days = (forecast_date - today).num_days().max(0) as u32;

        results.push(WeatherForecast {
            city: city.name.to_string(),
            model,
            forecast_date,
            max_temp_c,
            min_temp_c,
            prob_precip,
            ensemble_members: None,
            fetched_at,
            lead_days,
        });
    }
    Ok(results)
}

// ── Parse ensemble response ───────────────────────────────────────────────────
//
// Open-Meteo changed the ensemble API format: each member's data is now a
// separate column (`temperature_2m_max_member01` … `temperature_2m_max_member30`)
// rather than repeated rows with a shared `member` identifier.
//
// New column-based layout (2 members, 3 days):
//   time                       = ["2026-06-01", "2026-06-02", "2026-06-03"]
//   temperature_2m_max         = [25.5, 26.1, 27.0]   ← ensemble control/mean
//   temperature_2m_max_member01 = [25.1, 26.3, 27.2]
//   temperature_2m_max_member02 = [25.9, 25.8, 26.8]
//
// For each date at index i we collect all memberXX[i] values into
// ensemble_members so that ensemble_prob_in_range() has 30 readings per day.

pub(crate) fn parse_ensemble_response(
    raw: &EnsembleResponse,
    city: &CityInfo,
    days: u32,
) -> Result<Vec<WeatherForecast>, AppError> {
    let times = &raw.daily.time;
    let mean_max = &raw.daily.temperature_2m_max;

    if times.is_empty() || mean_max.is_empty() {
        return Err(AppError::ApiError(format!(
            "[OpenMeteo/Ensemble] empty response for {}", city.name
        )));
    }

    // Collect all per-member max-temp columns, sorted for consistency.
    let mut max_member_keys: Vec<&String> = raw.daily.extra.keys()
        .filter(|k| k.starts_with("temperature_2m_max_member"))
        .collect();
    max_member_keys.sort();

    let mut min_member_keys: Vec<&String> = raw.daily.extra.keys()
        .filter(|k| k.starts_with("temperature_2m_min_member"))
        .collect();
    min_member_keys.sort();

    if max_member_keys.is_empty() {
        return Err(AppError::ApiError(format!(
            "[OpenMeteo/Ensemble] no member columns in response for {} \
             (expected temperature_2m_max_member01 … )", city.name
        )));
    }

    // Parse each member column into a Vec<Option<f64>>.
    let parse_col = |key: &String| -> Vec<Option<f64>> {
        raw.daily.extra.get(key)
            .and_then(|v| serde_json::from_value::<Vec<Option<f64>>>(v.clone()).ok())
            .unwrap_or_default()
    };

    let max_cols: Vec<Vec<Option<f64>>> = max_member_keys.iter().map(|k| parse_col(k)).collect();
    let min_cols: Vec<Vec<Option<f64>>> = min_member_keys.iter().map(|k| parse_col(k)).collect();

    let fetched_at = Utc::now();
    let today = fetched_at.date_naive();

    let n = times.len().min(days as usize);
    let mut results: Vec<WeatherForecast> = Vec::with_capacity(n);

    for i in 0..n {
        let date_str = &times[i];
        let forecast_date = NaiveDate::parse_from_str(date_str, "%Y-%m-%d")
            .map_err(|e| AppError::ApiError(format!(
                "[OpenMeteo/Ensemble] bad date '{date_str}': {e}"
            )))?;

        // Collect all member readings for this date index.
        let member_maxes: Vec<f64> = max_cols.iter()
            .filter_map(|col| col.get(i).and_then(|v| *v))
            .collect();

        if member_maxes.is_empty() {
            tracing::debug!(
                "[OpenMeteo/Ensemble] {} date={date_str} has no member data, skipping",
                city.name
            );
            continue;
        }

        let mean_max_val = mean_max.get(i).copied().unwrap_or_else(|| {
            member_maxes.iter().sum::<f64>() / member_maxes.len() as f64
        });

        let mean_min = if !min_cols.is_empty() {
            let min_vals: Vec<f64> = min_cols.iter()
                .filter_map(|col| col.get(i).and_then(|v| *v))
                .collect();
            if min_vals.is_empty() {
                mean_max_val - 8.0
            } else {
                min_vals.iter().sum::<f64>() / min_vals.len() as f64
            }
        } else {
            mean_max_val - 8.0
        };

        let lead_days = (forecast_date - today).num_days().max(0) as u32;

        results.push(WeatherForecast {
            city: city.name.to_string(),
            model: WeatherModel::Ensemble,
            forecast_date,
            max_temp_c: mean_max_val,
            min_temp_c: mean_min,
            prob_precip: 0.0,
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
    /// Nullable: ECMWF IFS sometimes returns null for days beyond its horizon.
    pub temperature_2m_max: Vec<Option<f64>>,
    pub temperature_2m_min: Vec<Option<f64>>,
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
    /// Ensemble control / mean for the day (used as fallback mean_max).
    pub temperature_2m_max: Vec<f64>,
    pub temperature_2m_min: Option<Vec<f64>>,
    /// Per-member columns: `temperature_2m_max_member01` … captured dynamically.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
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
                temperature_2m_max: max_t.iter().map(|&v| Some(v)).collect(),
                temperature_2m_min: min_t.iter().map(|&v| Some(v)).collect(),
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
    fn parses_ensemble_response_column_format() {
        // New column-based format: 3 members × 2 days
        // temperature_2m_max_memberXX each have one value per date.
        let make_col = |vals: &[f64]| -> serde_json::Value {
            serde_json::to_value(vals.iter().map(|&v| Some(v)).collect::<Vec<_>>()).unwrap()
        };
        let raw = EnsembleResponse {
            daily: EnsembleDaily {
                time: vec!["2026-06-15".into(), "2026-06-16".into()],
                temperature_2m_max: vec![25.0, 26.0], // ensemble control
                temperature_2m_min: None,
                extra: [
                    ("temperature_2m_max_member01".to_string(), make_col(&[25.0, 26.0])),
                    ("temperature_2m_max_member02".to_string(), make_col(&[27.0, 28.0])),
                    ("temperature_2m_max_member03".to_string(), make_col(&[23.0, 24.0])),
                ].into_iter().collect(),
            },
        };
        let forecasts = parse_ensemble_response(&raw, nyc(), 2).unwrap();
        assert_eq!(forecasts.len(), 2);

        let f0 = forecasts.iter().find(|f| f.forecast_date.day() == 15).unwrap();
        assert_eq!(f0.member_count(), 3);
        // control mean = 25.0
        assert!((f0.max_temp_c - 25.0).abs() < 1e-6);

        // members: 25, 27, 23 → 25 ✓, 27 ✓, 23 ✗ → 2/3
        let p = f0.ensemble_prob_in_range(24.0, 27.0).unwrap();
        assert!((p - 2.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn ensemble_no_member_columns_returns_error() {
        let raw = EnsembleResponse {
            daily: EnsembleDaily {
                time: vec!["2026-06-15".into()],
                temperature_2m_max: vec![25.0],
                temperature_2m_min: None,
                extra: HashMap::new(), // no memberXX columns
            },
        };
        assert!(parse_ensemble_response(&raw, nyc(), 1).is_err());
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
