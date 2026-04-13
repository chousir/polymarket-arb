// Weather API module — multi-source forecast fetching.
//
// Three data sources are implemented as sub-modules:
//   openmeteo  — Open-Meteo REST API (GFS, ECMWF IFS, GFS Ensemble) — global, free, no key
//   nws        — NOAA/NWS API (US cities only) — free, no key
//   metar      — Aviation weather real-time observations — global, free, no key
//
// All sources output `WeatherForecast`, which is the common currency used by
// the decision and filter layers.

pub mod metar;
pub mod nws;
pub mod openmeteo;

use chrono::{DateTime, NaiveDate, Utc};

// ── WeatherModel ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeatherModel {
    /// NOAA National Weather Service (US only, contains official probability data)
    Nws,
    /// NOAA Global Forecast System — 6-hourly, global coverage
    Gfs,
    /// ECMWF Integrated Forecast System — best medium-range accuracy
    Ecmwf,
    /// GFS Ensemble — 30+ members, gives direct probability distribution
    Ensemble,
    /// Consensus of GFS + ECMWF + Ensemble probabilities
    Consensus,
    /// METAR + short-term NWS/GFS blended forecast for <=24h markets
    MetarShort,
    /// METAR real-time surface observation (not a forecast; lead_days = 0)
    Metar,
}

impl std::fmt::Display for WeatherModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WeatherModel::Nws      => write!(f, "nws"),
            WeatherModel::Gfs      => write!(f, "gfs"),
            WeatherModel::Ecmwf    => write!(f, "ecmwf"),
            WeatherModel::Ensemble => write!(f, "ensemble"),
            WeatherModel::Consensus => write!(f, "consensus"),
            WeatherModel::MetarShort => write!(f, "metar_short"),
            WeatherModel::Metar    => write!(f, "metar"),
        }
    }
}

// ── WeatherForecast ───────────────────────────────────────────────────────────

/// Unified forecast struct produced by all weather data sources.
///
/// For METAR observations `lead_days = 0` and `min_temp_c` / `max_temp_c` both
/// hold the current temperature.  `prob_precip` is set to 0.0 for METAR.
#[derive(Debug, Clone)]
pub struct WeatherForecast {
    /// Canonical city name ("NYC", "London", …)
    pub city: String,
    pub model: WeatherModel,
    /// Calendar date this forecast is valid for (local time at the city)
    pub forecast_date: NaiveDate,
    /// Predicted daily maximum temperature (°C)
    pub max_temp_c: f64,
    /// Predicted daily minimum temperature (°C)
    pub min_temp_c: f64,
    /// Probability of precipitation (0.0–1.0)
    pub prob_precip: f64,
    /// Ensemble member max-temp values (°C) — only populated by the Ensemble source.
    /// Each element is the max_temp_c prediction of one ensemble member.
    /// Use these to compute percentile-based probability distributions.
    pub ensemble_members: Option<Vec<f64>>,
    pub fetched_at: DateTime<Utc>,
    /// How many days ahead this forecast was made (0 = observation / same-day)
    pub lead_days: u32,
}

impl WeatherForecast {
    /// Returns the number of ensemble members, or 0 if not an ensemble forecast.
    pub fn member_count(&self) -> usize {
        self.ensemble_members.as_deref().map_or(0, |m| m.len())
    }

    /// Compute the fraction of ensemble members whose max_temp falls in [low, high].
    /// Returns None if this is not an ensemble forecast.
    pub fn ensemble_prob_in_range(&self, low: f64, high: f64) -> Option<f64> {
        let members = self.ensemble_members.as_deref()?;
        if members.is_empty() {
            return None;
        }
        let count = members.iter().filter(|&&t| t >= low && t <= high).count();
        Some(count as f64 / members.len() as f64)
    }

    /// Compute the fraction of ensemble members exceeding `threshold`.
    pub fn ensemble_prob_above(&self, threshold: f64) -> Option<f64> {
        let members = self.ensemble_members.as_deref()?;
        if members.is_empty() {
            return None;
        }
        let count = members.iter().filter(|&&t| t > threshold).count();
        Some(count as f64 / members.len() as f64)
    }
}

// ── CityInfo ──────────────────────────────────────────────────────────────────

/// Static metadata for a target city.
#[derive(Debug, Clone)]
pub struct CityInfo {
    /// Canonical short name used as the map key and stored in DB ("NYC", "London", …)
    pub name: &'static str,
    pub lat: f64,
    pub lon: f64,
    /// ICAO airport code for METAR lookups
    pub icao: &'static str,
    /// NWS Weather Forecast Office identifier — None for non-US cities
    pub nws_office: Option<&'static str>,
    /// IANA timezone name, used to align forecast dates to local calendar
    pub timezone: &'static str,
}

/// Look up city metadata by name (case-insensitive).
pub fn city_info(name: &str) -> Option<&'static CityInfo> {
    let lower = name.to_lowercase();
    ALL_CITIES.iter().find(|c| c.name.to_lowercase() == lower)
}

/// All supported target cities, ordered by expected market liquidity.
pub static ALL_CITIES: &[CityInfo] = &[
    // ── United States ─────────────────────────────────────────────────────────
    CityInfo {
        name: "NYC",
        lat: 40.7128, lon: -74.0060,
        icao: "KJFK",
        nws_office: Some("OKX"),
        timezone: "America/New_York",
    },
    CityInfo {
        name: "Miami",
        lat: 25.7617, lon: -80.1918,
        icao: "KMIA",
        nws_office: Some("MFL"),
        timezone: "America/New_York",
    },
    CityInfo {
        name: "Chicago",
        lat: 41.8781, lon: -87.6298,
        icao: "KORD",
        nws_office: Some("LOT"),
        timezone: "America/Chicago",
    },
    CityInfo {
        name: "LA",
        lat: 34.0522, lon: -118.2437,
        icao: "KLAX",
        nws_office: Some("LOX"),
        timezone: "America/Los_Angeles",
    },
    CityInfo {
        name: "Houston",
        lat: 29.7604, lon: -95.3698,
        icao: "KIAH",
        nws_office: Some("HGX"),
        timezone: "America/Chicago",
    },
    CityInfo {
        name: "Phoenix",
        lat: 33.4484, lon: -112.0740,
        icao: "KPHX",
        nws_office: Some("PSR"),
        timezone: "America/Phoenix",
    },
    CityInfo {
        name: "San Francisco",
        lat: 37.7749, lon: -122.4194,
        icao: "KSFO",
        nws_office: Some("MTR"),
        timezone: "America/Los_Angeles",
    },
    CityInfo {
        name: "Boston",
        lat: 42.3601, lon: -71.0589,
        icao: "KBOS",
        nws_office: Some("BOX"),
        timezone: "America/New_York",
    },
    CityInfo {
        name: "Seattle",
        lat: 47.6062, lon: -122.3321,
        icao: "KSEA",
        nws_office: Some("SEW"),
        timezone: "America/Los_Angeles",
    },
    CityInfo {
        name: "Atlanta",
        lat: 33.7490, lon: -84.3880,
        icao: "KATL",
        nws_office: Some("FFC"),
        timezone: "America/New_York",
    },
    CityInfo {
        name: "Dallas",
        lat: 32.7767, lon: -96.7970,
        icao: "KDFW",
        nws_office: Some("FWD"),
        timezone: "America/Chicago",
    },
    CityInfo {
        name: "Denver",
        lat: 39.7392, lon: -104.9903,
        icao: "KDEN",
        nws_office: Some("BOU"),
        timezone: "America/Denver",
    },
    // ── Canada ────────────────────────────────────────────────────────────────
    CityInfo {
        name: "Toronto",
        lat: 43.6532, lon: -79.3832,
        icao: "CYYZ",
        nws_office: None,
        timezone: "America/Toronto",
    },
    // ── Europe ────────────────────────────────────────────────────────────────
    CityInfo {
        name: "London",
        lat: 51.5074, lon: -0.1278,
        icao: "EGLL",
        nws_office: None,
        timezone: "Europe/London",
    },
    CityInfo {
        name: "Paris",
        lat: 48.8566, lon: 2.3522,
        icao: "LFPG",
        nws_office: None,
        timezone: "Europe/Paris",
    },
    CityInfo {
        name: "Berlin",
        lat: 52.5200, lon: 13.4050,
        icao: "EDDB",
        nws_office: None,
        timezone: "Europe/Berlin",
    },
    CityInfo {
        name: "Amsterdam",
        lat: 52.3676, lon: 4.9041,
        icao: "EHAM",
        nws_office: None,
        timezone: "Europe/Amsterdam",
    },
    CityInfo {
        name: "Madrid",
        lat: 40.4168, lon: -3.7038,
        icao: "LEMD",
        nws_office: None,
        timezone: "Europe/Madrid",
    },
    CityInfo {
        name: "Rome",
        lat: 41.9028, lon: 12.4964,
        icao: "LIRF",
        nws_office: None,
        timezone: "Europe/Rome",
    },
    // ── Asia ──────────────────────────────────────────────────────────────────
    CityInfo {
        name: "Tokyo",
        lat: 35.6762, lon: 139.6503,
        icao: "RJTT",
        nws_office: None,
        timezone: "Asia/Tokyo",
    },
    CityInfo {
        name: "Seoul",
        lat: 37.5665, lon: 126.9780,
        icao: "RKSS",
        nws_office: None,
        timezone: "Asia/Seoul",
    },
    CityInfo {
        name: "Dubai",
        lat: 25.2048, lon: 55.2708,
        icao: "OMDB",
        nws_office: None,
        timezone: "Asia/Dubai",
    },
    CityInfo {
        name: "Singapore",
        lat: 1.3521, lon: 103.8198,
        icao: "WSSS",
        nws_office: None,
        timezone: "Asia/Singapore",
    },
    CityInfo {
        name: "Hong Kong",
        lat: 22.3193, lon: 114.1694,
        icao: "VHHH",
        nws_office: None,
        timezone: "Asia/Hong_Kong",
    },
    CityInfo {
        name: "Bangkok",
        lat: 13.7563, lon: 100.5018,
        icao: "VTBS",
        nws_office: None,
        timezone: "Asia/Bangkok",
    },
    CityInfo {
        name: "Mumbai",
        lat: 19.0760, lon: 72.8777,
        icao: "VABB",
        nws_office: None,
        timezone: "Asia/Kolkata",
    },
    // ── Oceania ───────────────────────────────────────────────────────────────
    CityInfo {
        name: "Sydney",
        lat: -33.8688, lon: 151.2093,
        icao: "YSSY",
        nws_office: None,
        timezone: "Australia/Sydney",
    },
    CityInfo {
        name: "Melbourne",
        lat: -37.8136, lon: 144.9631,
        icao: "YMML",
        nws_office: None,
        timezone: "Australia/Melbourne",
    },
];

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn city_lookup_case_insensitive() {
        assert!(city_info("NYC").is_some());
        assert!(city_info("nyc").is_some());
        assert!(city_info("london").is_some());
        assert!(city_info("LONDON").is_some());
        assert!(city_info("nonexistent").is_none());
    }

    #[test]
    fn us_cities_have_nws_office() {
        for name in &["NYC", "Miami", "Chicago", "LA"] {
            let info = city_info(name).expect("city must exist");
            assert!(info.nws_office.is_some(), "{name} must have NWS office");
        }
    }

    #[test]
    fn non_us_cities_have_no_nws_office() {
        for name in &["London", "Tokyo", "Dubai", "Sydney"] {
            let info = city_info(name).expect("city must exist");
            assert!(info.nws_office.is_none(), "{name} must not have NWS office");
        }
    }

    #[test]
    fn ensemble_prob_in_range() {
        let fc = WeatherForecast {
            city: "NYC".to_string(),
            model: WeatherModel::Ensemble,
            forecast_date: chrono::NaiveDate::from_ymd_opt(2026, 6, 15).unwrap(),
            max_temp_c: 28.0,
            min_temp_c: 18.0,
            prob_precip: 0.1,
            ensemble_members: Some(vec![25.0, 27.0, 29.0, 31.0, 26.0, 28.0, 30.0, 24.0]),
            fetched_at: Utc::now(),
            lead_days: 3,
        };
        // 27, 29, 28 → 3 out of 8 members in [27, 30] range
        let p = fc.ensemble_prob_in_range(27.0, 30.0).unwrap();
        // members in [27, 30]: 27.0, 29.0, 28.0, 30.0 = 4/8 = 0.5
        assert!((p - 0.5).abs() < 1e-9, "expected 0.5 got {p}");
    }

    #[test]
    fn ensemble_prob_above() {
        let fc = WeatherForecast {
            city: "Dubai".to_string(),
            model: WeatherModel::Ensemble,
            forecast_date: chrono::NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
            max_temp_c: 42.0,
            min_temp_c: 30.0,
            prob_precip: 0.0,
            // 3 out of 5 above 40
            ensemble_members: Some(vec![38.0, 41.0, 43.0, 39.0, 42.0]),
            fetched_at: Utc::now(),
            lead_days: 1,
        };
        let p = fc.ensemble_prob_above(40.0).unwrap();
        assert!((p - 0.6).abs() < 1e-9, "expected 0.6 got {p}");
    }

    #[test]
    fn member_count_none_returns_zero() {
        let fc = WeatherForecast {
            city: "London".to_string(),
            model: WeatherModel::Gfs,
            forecast_date: chrono::NaiveDate::from_ymd_opt(2026, 4, 20).unwrap(),
            max_temp_c: 18.0,
            min_temp_c: 10.0,
            prob_precip: 0.3,
            ensemble_members: None,
            fetched_at: Utc::now(),
            lead_days: 7,
        };
        assert_eq!(fc.member_count(), 0);
    }
}
