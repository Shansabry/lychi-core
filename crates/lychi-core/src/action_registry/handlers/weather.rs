use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use chrono::NaiveDateTime;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, OutputType,
    RiskLevel,
};
use crate::error::LychiError;

const USER_AGENT: &str = "Lychi/1.0 (https://lychi.app)";
const GEOCODE_URL: &str = "https://nominatim.openstreetmap.org/search";
const FORECAST_URL: &str = "https://api.met.no/weatherapi/locationforecast/2.0/compact";
const IP_GEO_URL: &str = "https://ipwho.is/";

/// Cache TTL for weather data (10 minutes).
const WEATHER_CACHE_SECS: u64 = 600;

// --- Nominatim geocoding response ---

#[derive(Debug, Deserialize)]
struct GeoResult {
    #[serde(deserialize_with = "de_f64_from_str")]
    lat: f64,
    #[serde(deserialize_with = "de_f64_from_str")]
    lon: f64,
    display_name: String,
}

fn de_f64_from_str<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    let s = String::deserialize(d)?;
    s.parse().map_err(serde::de::Error::custom)
}

// --- MET Norway locationforecast response (compact) ---

#[derive(Debug, Deserialize)]
struct Forecast {
    properties: ForecastProperties,
}

#[derive(Debug, Deserialize)]
struct ForecastProperties {
    timeseries: Vec<ForecastEntry>,
}

#[derive(Debug, Deserialize)]
struct ForecastEntry {
    time: String,
    data: ForecastData,
}

#[derive(Debug, Deserialize)]
struct ForecastData {
    instant: ForecastInstant,
    next_1_hours: Option<ForecastNextHours>,
    next_6_hours: Option<ForecastNextHours>,
}

#[derive(Debug, Deserialize)]
struct ForecastInstant {
    details: ForecastDetails,
}

#[derive(Debug, Deserialize)]
struct ForecastDetails {
    air_temperature: f64,
    relative_humidity: Option<f64>,
    wind_speed: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct ForecastNextHours {
    summary: ForecastSummary,
}

#[derive(Debug, Deserialize)]
struct ForecastSummary {
    symbol_code: String,
}

// --- IP geolocation response (ipwho.is) ---

#[derive(Debug, Deserialize)]
struct IpGeoResult {
    latitude: f64,
    longitude: f64,
    city: String,
}

/// Time/self-reference qualifiers that mean "here, right now" rather than a
/// place. Stripping them makes the location auto-detect (IP geolocation) instead
/// of geocoding the word as a place name. The agent passes natural phrasings
/// ("weather now", "weather today") straight through, so we normalize them here.
const LOCAL_QUALIFIERS: &[&str] = &[
    "here",
    "now",
    "today",
    "right now",
    "current",
    "currently",
    "my location",
    "current location",
    "local",
    "outside",
];

/// Normalize a weather location argument: trim, and if it's purely a "here/now"
/// qualifier (or empty), return "" to trigger auto-detect. Otherwise return the
/// place as typed ("tokyo" → "tokyo", "weather now" → auto-detect).
fn normalize_weather_location(args: &str) -> &str {
    let t = args.trim();
    let lower = t.to_lowercase();
    if lower.is_empty() || LOCAL_QUALIFIERS.contains(&lower.as_str()) {
        return "";
    }
    t
}

// --- Structured output for frontend ---

#[derive(Debug, Serialize)]
pub struct WeatherData {
    pub location: String,
    pub unit: String,
    pub current: CurrentWeather,
    pub forecast: Vec<DayForecast>,
}

#[derive(Debug, Serialize)]
pub struct CurrentWeather {
    pub temp: i32,
    pub humidity: Option<f64>,
    pub wind_speed: Option<f64>,
    pub condition: String,
    pub symbol: String,
}

#[derive(Debug, Serialize)]
pub struct DayForecast {
    pub day: String,
    pub temp_high: i32,
    pub temp_low: i32,
    pub symbol: String,
    pub condition: String,
}

// --- Caches ---

struct GeoCache {
    coords: (f64, f64),
    display_name: String,
}

struct WeatherCache {
    output: String,
    fetched_at: Instant,
}

pub struct WeatherHandler {
    unit: String,
    default_location: String,
    client: Client,
    geo_cache: Arc<RwLock<HashMap<String, GeoCache>>>,
    weather_cache: Arc<RwLock<HashMap<String, WeatherCache>>>,
}

impl WeatherHandler {
    pub fn new(unit: String, default_location: String) -> Self {
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .unwrap_or_default();

        Self {
            unit,
            default_location,
            client,
            geo_cache: Arc::new(RwLock::new(HashMap::new())),
            weather_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Auto-detect location from IP address using ipwho.is (free, HTTPS, no key).
    async fn detect_location(&self) -> Result<(f64, f64, String), LychiError> {
        let geo: IpGeoResult = self
            .client
            .get(IP_GEO_URL)
            .send()
            .await
            .map_err(|e| {
                LychiError::ExecutionFailed(format!("IP geolocation request failed: {e}"))
            })?
            .json()
            .await
            .map_err(|e| LychiError::ExecutionFailed(format!("IP geolocation parse error: {e}")))?;

        if geo.city.is_empty() {
            return Err(LychiError::ExecutionFailed(
                "Could not detect city from IP address".to_string(),
            ));
        }

        Ok((geo.latitude, geo.longitude, geo.city))
    }

    async fn geocode(&self, query: &str) -> Result<(f64, f64, String), LychiError> {
        let key = query.to_lowercase();

        if let Some(cached) = self.geo_cache.read().await.get(&key) {
            return Ok((
                cached.coords.0,
                cached.coords.1,
                cached.display_name.clone(),
            ));
        }

        let results: Vec<GeoResult> = self
            .client
            .get(GEOCODE_URL)
            .query(&[("q", query), ("format", "json"), ("limit", "1")])
            .send()
            .await
            .map_err(|e| LychiError::ExecutionFailed(format!("Geocoding request failed: {e}")))?
            .json()
            .await
            .map_err(|e| LychiError::ExecutionFailed(format!("Geocoding parse error: {e}")))?;

        let geo = results
            .into_iter()
            .next()
            .ok_or_else(|| LychiError::ExecutionFailed(format!("Location not found: {query}")))?;

        self.geo_cache.write().await.insert(
            key,
            GeoCache {
                coords: (geo.lat, geo.lon),
                display_name: geo.display_name.clone(),
            },
        );

        Ok((geo.lat, geo.lon, geo.display_name))
    }

    async fn fetch_weather(&self, lat: f64, lon: f64) -> Result<Forecast, LychiError> {
        let lat = (lat * 10000.0).round() / 10000.0;
        let lon = (lon * 10000.0).round() / 10000.0;

        self.client
            .get(FORECAST_URL)
            .query(&[("lat", lat.to_string()), ("lon", lon.to_string())])
            .send()
            .await
            .map_err(|e| LychiError::ExecutionFailed(format!("Weather request failed: {e}")))?
            .json::<Forecast>()
            .await
            .map_err(|e| LychiError::ExecutionFailed(format!("Weather parse error: {e}")))
    }

    fn convert_temp(&self, celsius: f64) -> i32 {
        if self.unit == "fahrenheit" {
            (celsius * 9.0 / 5.0 + 32.0).round() as i32
        } else {
            celsius.round() as i32
        }
    }

    fn build_weather_data(&self, location: &str, forecast: &Forecast) -> WeatherData {
        let timeseries = &forecast.properties.timeseries;

        // Current weather from first entry
        let current = timeseries
            .first()
            .map(|entry| {
                let d = &entry.data.instant.details;
                let symbol = entry
                    .data
                    .next_1_hours
                    .as_ref()
                    .or(entry.data.next_6_hours.as_ref())
                    .map(|h| h.summary.symbol_code.clone())
                    .unwrap_or_default();
                CurrentWeather {
                    temp: self.convert_temp(d.air_temperature),
                    humidity: d.relative_humidity,
                    wind_speed: d.wind_speed,
                    condition: symbol_to_description(&symbol).to_string(),
                    symbol: strip_time_suffix(&symbol).to_string(),
                }
            })
            .unwrap_or(CurrentWeather {
                temp: 0,
                humidity: None,
                wind_speed: None,
                condition: "unknown".to_string(),
                symbol: "unknown".to_string(),
            });

        // 3-day forecast: group by date, compute high/low/symbol
        let today = timeseries
            .first()
            .and_then(|e| parse_date(&e.time))
            .unwrap_or_default();

        let mut day_data: HashMap<String, (f64, f64, String)> = HashMap::new(); // date → (min, max, symbol)

        for entry in timeseries {
            let Some(date) = parse_date(&entry.time) else {
                continue;
            };
            if date <= today {
                continue; // skip today
            }

            let temp = entry.data.instant.details.air_temperature;
            let e = day_data.entry(date.clone()).or_insert((
                f64::INFINITY,
                f64::NEG_INFINITY,
                String::new(),
            ));
            e.0 = e.0.min(temp);
            e.1 = e.1.max(temp);

            // Pick symbol from entry closest to midday (12:00)
            if e.2.is_empty() || is_closer_to_noon(&entry.time, &e.2) {
                let symbol = entry
                    .data
                    .next_1_hours
                    .as_ref()
                    .or(entry.data.next_6_hours.as_ref())
                    .map(|h| h.summary.symbol_code.clone())
                    .unwrap_or_default();
                if !symbol.is_empty() {
                    e.2 = symbol;
                }
            }
        }

        let mut forecast_days: Vec<(String, f64, f64, String)> = day_data
            .into_iter()
            .map(|(date, (lo, hi, sym))| (date, lo, hi, sym))
            .collect();
        forecast_days.sort_by(|a, b| a.0.cmp(&b.0));
        forecast_days.truncate(3);

        let forecast_out: Vec<DayForecast> = forecast_days
            .into_iter()
            .map(|(date, lo, hi, sym)| {
                let day_name = weekday_name(&date);
                DayForecast {
                    day: day_name,
                    temp_high: self.convert_temp(hi),
                    temp_low: self.convert_temp(lo),
                    condition: symbol_to_description(&sym).to_string(),
                    symbol: strip_time_suffix(&sym).to_string(),
                }
            })
            .collect();

        WeatherData {
            location: location.to_string(),
            unit: if self.unit == "fahrenheit" {
                "F".to_string()
            } else {
                "C".to_string()
            },
            current,
            forecast: forecast_out,
        }
    }

    /// Fetch structured weather data for a location.
    /// Used by WeatherAskHandler to get data for AI-powered answers.
    pub async fn get_weather_data(&self, location: &str) -> Result<WeatherData, LychiError> {
        let location_str = if !location.is_empty() {
            location.to_string()
        } else if !self.default_location.is_empty() {
            self.default_location.clone()
        } else {
            String::new()
        };

        let (lat, lon, display_name) = if location_str.is_empty() {
            // C6: This path is gated by the Rules Engine — the user must consent
            // to IP geolocation (privacy.allow_ip_geolocation) before reaching here.
            self.detect_location().await?
        } else {
            self.geocode(&location_str).await?
        };

        let forecast = self.fetch_weather(lat, lon).await?;
        let short = Self::short_name(&display_name);
        Ok(self.build_weather_data(short, &forecast))
    }

    fn short_name(display_name: &str) -> &str {
        display_name
            .split(',')
            .next()
            .unwrap_or(display_name)
            .trim()
    }
}

#[async_trait]
impl ActionHandler for WeatherHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::{ArgTransform, Trigger};
        static TRIGGERS: &[Trigger] = &[Trigger::new(
            &["weather"],
            ArgTransform::StripLeading("in "),
        )];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "weather"
    }

    fn description(&self) -> &str {
        "Get current weather for a location"
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Utilities
    }

    fn default_risk(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        // Normalize the location argument. Words that mean "here / right now" —
        // not a place — resolve to auto-detect (IP geolocation), so "weather now"
        // and "weather here" both give the LOCAL weather instead of geocoding the
        // qualifier as a place name (which mis-resolved "now" → a random city).
        let input = normalize_weather_location(args);

        let cache_key = if input.is_empty() {
            "__auto__".to_string()
        } else {
            input.to_lowercase()
        };

        // Check weather cache
        if let Some(cached) = self.weather_cache.read().await.get(&cache_key)
            && cached.fetched_at.elapsed().as_secs() < WEATHER_CACHE_SECS
        {
            return Ok(ActionResult::ok(cached.output.clone(), OutputType::Weather));
        }

        let start = Instant::now();
        let weather_data = self.get_weather_data(input).await?;
        let output = serde_json::to_string(&weather_data)
            .map_err(|e| LychiError::ExecutionFailed(format!("JSON serialize error: {e}")))?;
        let duration_ms = start.elapsed().as_millis() as u64;

        self.weather_cache.write().await.insert(
            cache_key,
            WeatherCache {
                output: output.clone(),
                fetched_at: Instant::now(),
            },
        );

        Ok(ActionResult::ok(output, OutputType::Weather).with_duration(duration_ms))
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let lower = partial.to_lowercase();
        let mut items = Vec::new();
        if "here".contains(&lower) || lower.is_empty() {
            items.push(CompletionItem {
                label: "here".to_string(),
                icon_path: None,
                score: 100,
                description: Some("Detect current location".to_string()),
                reason: None,
                thumb_b64: None,
                run: Some("weather here".to_string()),
                ..Default::default()
            });
        }
        items
    }
}

/// Allow `Arc<WeatherHandler>` to be registered in the ActionRegistry
/// while sharing the same instance with WeatherAskHandler.
#[async_trait]
impl ActionHandler for Arc<WeatherHandler> {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        self.as_ref().triggers()
    }
    fn id(&self) -> &str {
        self.as_ref().id()
    }
    fn description(&self) -> &str {
        self.as_ref().description()
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Utilities
    }
    fn default_risk(&self) -> RiskLevel {
        self.as_ref().default_risk()
    }
    async fn execute(&self, ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        self.as_ref().execute(ctx, args).await
    }
    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        self.as_ref().completions(partial).await
    }
}

// --- Helper functions ---

/// Extract date portion (YYYY-MM-DD) from ISO 8601 timestamp.
fn parse_date(time: &str) -> Option<String> {
    time.get(..10).map(|s| s.to_string())
}

/// Check if a timestamp is closer to 12:00 than the existing symbol's implicit time.
/// Simple heuristic: prefer entries with hour 10-14.
fn is_closer_to_noon(time: &str, _existing: &str) -> bool {
    let hour = time
        .get(11..13)
        .and_then(|h| h.parse::<u32>().ok())
        .unwrap_or(0);
    (10..=14).contains(&hour)
}

/// Strip _day/_night/_polartwilight suffix from symbol code.
fn strip_time_suffix(code: &str) -> &str {
    code.strip_suffix("_day")
        .or_else(|| code.strip_suffix("_night"))
        .or_else(|| code.strip_suffix("_polartwilight"))
        .unwrap_or(code)
}

/// Get short weekday name from YYYY-MM-DD date string.
fn weekday_name(date: &str) -> String {
    NaiveDateTime::parse_from_str(&format!("{date}T00:00:00"), "%Y-%m-%dT%H:%M:%S")
        .map(|dt| dt.format("%a").to_string())
        .unwrap_or_else(|_| date.to_string())
}

/// Convert MET Norway symbol codes to human-readable descriptions.
fn symbol_to_description(code: &str) -> &str {
    let base = strip_time_suffix(code);

    match base {
        "clearsky" => "clear sky",
        "fair" => "fair",
        "partlycloudy" => "partly cloudy",
        "cloudy" => "cloudy",
        "lightrainshowers" => "light rain showers",
        "rainshowers" => "rain showers",
        "heavyrainshowers" => "heavy rain showers",
        "lightrainshowersandthunder" => "light rain showers and thunder",
        "rainshowersandthunder" => "rain showers and thunder",
        "heavyrainshowersandthunder" => "heavy rain showers and thunder",
        "lightsleetshowers" => "light sleet showers",
        "sleetshowers" => "sleet showers",
        "heavysleetshowers" => "heavy sleet showers",
        "lightsnowshowers" => "light snow showers",
        "snowshowers" => "snow showers",
        "heavysnowshowers" => "heavy snow showers",
        "lightrain" => "light rain",
        "rain" => "rain",
        "heavyrain" => "heavy rain",
        "lightrainandthunder" => "light rain and thunder",
        "rainandthunder" => "rain and thunder",
        "heavyrainandthunder" => "heavy rain and thunder",
        "lightsleet" => "light sleet",
        "sleet" => "sleet",
        "heavysleet" => "heavy sleet",
        "lightsnow" => "light snow",
        "snow" => "snow",
        "heavysnow" => "heavy snow",
        "fog" => "fog",
        _ => code,
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_weather_location;

    #[test]
    fn local_qualifiers_resolve_to_autodetect() {
        for q in [
            "here",
            "now",
            "today",
            "right now",
            "current",
            "currently",
            "my location",
            "local",
            "NOW",
            " Here ",
            "",
        ] {
            assert_eq!(
                normalize_weather_location(q),
                "",
                "{q:?} should auto-detect"
            );
        }
    }

    #[test]
    fn real_places_pass_through() {
        assert_eq!(normalize_weather_location("tokyo"), "tokyo");
        assert_eq!(normalize_weather_location("  Nagercoil "), "Nagercoil");
        assert_eq!(normalize_weather_location("new york"), "new york");
    }
}
