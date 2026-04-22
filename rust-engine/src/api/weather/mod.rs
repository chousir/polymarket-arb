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
            WeatherModel::Nws        => write!(f, "nws"),
            WeatherModel::Gfs        => write!(f, "gfs"),
            WeatherModel::Ecmwf      => write!(f, "ecmwf"),
            WeatherModel::Ensemble   => write!(f, "ensemble"),
            WeatherModel::Consensus  => write!(f, "consensus"),
            WeatherModel::MetarShort => write!(f, "metar_short"),
            WeatherModel::Metar      => write!(f, "metar"),
        }
    }
}

impl std::str::FromStr for WeatherModel {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "nws"         => Ok(WeatherModel::Nws),
            "gfs"         => Ok(WeatherModel::Gfs),
            "ecmwf"       => Ok(WeatherModel::Ecmwf),
            "ensemble"    => Ok(WeatherModel::Ensemble),
            "consensus"   => Ok(WeatherModel::Consensus),
            "metar_short" => Ok(WeatherModel::MetarShort),
            "metar"       => Ok(WeatherModel::Metar),
            _             => Err(()),
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
        // Polymarket resolves via KLGA (wunderground KLGA — LaGuardia).
        name: "NYC",
        lat: 40.7772, lon: -73.8726,
        icao: "KLGA",
        nws_office: Some("OKX"),
        timezone: "America/New_York",
    },
    CityInfo {
        name: "Miami",
        lat: 25.7959, lon: -80.2870,
        icao: "KMIA",
        nws_office: Some("MFL"),
        timezone: "America/New_York",
    },
    CityInfo {
        // KORD is 26km NW of downtown; airport coords give more accurate GFS/NWS forecast.
        name: "Chicago",
        lat: 41.9742, lon: -87.9073,
        icao: "KORD",
        nws_office: Some("LOT"),
        timezone: "America/Chicago",
    },
    CityInfo {
        // KLAX is coastal (33.94°N vs downtown 34.05°N).  Marine layer effect is
        // dramatically different 18km inland — using airport coords is essential.
        name: "LA",
        lat: 33.9425, lon: -118.4081,
        icao: "KLAX",
        nws_office: Some("LOX"),
        timezone: "America/Los_Angeles",
    },
    CityInfo {
        // KIAH (Bush IAH) is 26km north of downtown Houston.
        name: "Houston",
        lat: 29.9902, lon: -95.3368,
        icao: "KIAH",
        nws_office: Some("HGX"),
        timezone: "America/Chicago",
    },
    CityInfo {
        name: "Phoenix",
        lat: 33.4373, lon: -112.0078,
        icao: "KPHX",
        nws_office: Some("PSR"),
        timezone: "America/Phoenix",
    },
    CityInfo {
        // KSFO sits on the bay peninsula, measurably cooler than downtown SF.
        name: "San Francisco",
        lat: 37.6213, lon: -122.3790,
        icao: "KSFO",
        nws_office: Some("MTR"),
        timezone: "America/Los_Angeles",
    },
    CityInfo {
        name: "Boston",
        lat: 42.3601, lon: -71.0032,
        icao: "KBOS",
        nws_office: Some("BOX"),
        timezone: "America/New_York",
    },
    CityInfo {
        // KSEA (SeaTac) is 18km south of downtown Seattle.
        name: "Seattle",
        lat: 47.4502, lon: -122.3088,
        icao: "KSEA",
        nws_office: Some("SEW"),
        timezone: "America/Los_Angeles",
    },
    CityInfo {
        name: "Atlanta",
        lat: 33.6407, lon: -84.4277,
        icao: "KATL",
        nws_office: Some("FFC"),
        timezone: "America/New_York",
    },
    CityInfo {
        // KDFW is 32km NW of downtown Dallas.
        name: "Dallas",
        lat: 32.8968, lon: -97.0380,
        icao: "KDFW",
        nws_office: Some("FWD"),
        timezone: "America/Chicago",
    },
    CityInfo {
        // KDEN is 38km NE of downtown Denver, on the plains (lower elevation).
        name: "Denver",
        lat: 39.8561, lon: -104.6737,
        icao: "KDEN",
        nws_office: Some("BOU"),
        timezone: "America/Denver",
    },
    // ── Canada ────────────────────────────────────────────────────────────────
    CityInfo {
        // Polymarket resolves via CYYZ (wunderground CYYZ — Pearson International).
        name: "Toronto",
        lat: 43.6777, lon: -79.6248,
        icao: "CYYZ",
        nws_office: None,
        timezone: "America/Toronto",
    },
    // ── Europe ────────────────────────────────────────────────────────────────
    CityInfo {
        // Polymarket resolves via EGLC (wunderground EGLC — London City Airport).
        name: "London",
        lat: 51.5053, lon: 0.0553,
        icao: "EGLC",
        nws_office: None,
        timezone: "Europe/London",
    },
    CityInfo {
        // Polymarket resolves via LFPB (wunderground LFPB — Paris Le Bourget).
        name: "Paris",
        lat: 48.9694, lon: 2.4358,
        icao: "LFPB",
        nws_office: None,
        timezone: "Europe/Paris",
    },
    CityInfo {
        name: "Berlin",
        lat: 52.3672, lon: 13.5031,
        icao: "EDDB",
        nws_office: None,
        timezone: "Europe/Berlin",
    },
    CityInfo {
        name: "Amsterdam",
        lat: 52.3080, lon: 4.7642,
        icao: "EHAM",
        nws_office: None,
        timezone: "Europe/Amsterdam",
    },
    CityInfo {
        name: "Madrid",
        lat: 40.4981, lon: -3.5675,
        icao: "LEMD",
        nws_office: None,
        timezone: "Europe/Madrid",
    },
    CityInfo {
        name: "Rome",
        lat: 41.8003, lon: 12.2339,
        icao: "LIRF",
        nws_office: None,
        timezone: "Europe/Rome",
    },
    // ── Asia ──────────────────────────────────────────────────────────────────
    CityInfo {
        // Polymarket resolves via RJTT (wunderground RJTT — Tokyo Haneda).
        name: "Tokyo",
        lat: 35.5494, lon: 139.7798,
        icao: "RJTT",
        nws_office: None,
        timezone: "Asia/Tokyo",
    },
    CityInfo {
        name: "Seoul",
        lat: 37.5581, lon: 126.7911,
        icao: "RKSS",
        nws_office: None,
        timezone: "Asia/Seoul",
    },
    CityInfo {
        name: "Dubai",
        lat: 25.2528, lon: 55.3644,
        icao: "OMDB",
        nws_office: None,
        timezone: "Asia/Dubai",
    },
    CityInfo {
        name: "Singapore",
        lat: 1.3592, lon: 103.9894,
        icao: "WSSS",
        nws_office: None,
        timezone: "Asia/Singapore",
    },
    CityInfo {
        name: "Hong Kong",
        lat: 22.3089, lon: 113.9144,
        icao: "VHHH",
        nws_office: None,
        timezone: "Asia/Hong_Kong",
    },
    CityInfo {
        name: "Bangkok",
        lat: 13.6847, lon: 100.7469,
        icao: "VTBS",
        nws_office: None,
        timezone: "Asia/Bangkok",
    },
    CityInfo {
        name: "Mumbai",
        lat: 19.0947, lon: 72.8686,
        icao: "VABB",
        nws_office: None,
        timezone: "Asia/Kolkata",
    },
    // ── Oceania ───────────────────────────────────────────────────────────────
    CityInfo {
        name: "Sydney",
        lat: -33.9461, lon: 151.1772,
        icao: "YSSY",
        nws_office: None,
        timezone: "Australia/Sydney",
    },
    CityInfo {
        name: "Melbourne",
        lat: -37.6731, lon: 144.8425,
        icao: "YMML",
        nws_office: None,
        timezone: "Australia/Melbourne",
    },
    // ── Extended city list ────────────────────────────────────────────────────
    CityInfo {
        name: "Beijing",
        lat: 40.0800, lon: 116.5847,
        icao: "ZBAA",
        nws_office: None,
        timezone: "Asia/Shanghai",
    },
    CityInfo {
        name: "Moscow",
        lat: 55.9725, lon: 37.4139,
        icao: "UUEE",
        nws_office: None,
        timezone: "Europe/Moscow",
    },
    CityInfo {
        name: "São Paulo",
        lat: -23.4356, lon: -46.4728,
        icao: "SBGR",
        nws_office: None,
        timezone: "America/Sao_Paulo",
    },
    CityInfo {
        name: "Buenos Aires",
        lat: -34.8219, lon: -58.5353,
        icao: "SAEZ",
        nws_office: None,
        timezone: "America/Argentina/Buenos_Aires",
    },
    CityInfo {
        name: "Ankara",
        lat: 40.1283, lon: 32.9950,
        icao: "LTAC",
        nws_office: None,
        timezone: "Europe/Istanbul",
    },
    CityInfo {
        name: "Wellington",
        lat: -41.3272, lon: 174.8050,
        icao: "NZWN",
        nws_office: None,
        timezone: "Pacific/Auckland",
    },
    CityInfo {
        name: "Munich",
        lat: 48.3539, lon: 11.7861,
        icao: "EDDM",
        nws_office: None,
        timezone: "Europe/Berlin",
    },
    CityInfo {
        name: "Tel Aviv",
        lat: 32.0117, lon: 34.8850,
        icao: "LLBG",
        nws_office: None,
        timezone: "Asia/Jerusalem",
    },
    CityInfo {
        name: "Austin",
        lat: 30.1983, lon: -97.6700,
        icao: "KAUS",
        nws_office: Some("EWX"),
        timezone: "America/Chicago",
    },
    CityInfo {
        name: "Lucknow",
        lat: 26.7603, lon: 80.8892,
        icao: "VILK",
        nws_office: None,
        timezone: "Asia/Kolkata",
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
