use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::action_registry::{ActionHandler, ActionResult, ExecContext, OutputType, RiskLevel};
use crate::error::LychiError;
use crate::providers::AiProvider;

use super::weather::WeatherHandler;

const WEATHER_ASK_SYSTEM_PROMPT: &str = r#"You are a weather assistant in a desktop launcher. You have real-time weather data. Answer the user's weather question in 1-2 sentences. Be direct, conversational, and helpful. Do not use markdown formatting."#;

const WEATHER_ASK_TIMEOUT: Duration = Duration::from_secs(15);

pub struct WeatherAskHandler {
    weather: Arc<WeatherHandler>,
    ai_provider: Option<Arc<dyn AiProvider>>,
}

impl WeatherAskHandler {
    pub fn new(weather: Arc<WeatherHandler>, ai_provider: Option<Arc<dyn AiProvider>>) -> Self {
        Self {
            weather,
            ai_provider,
        }
    }

    /// Format weather data into a context string for the AI.
    fn format_context(data: &super::weather::WeatherData) -> String {
        let mut ctx = format!(
            "Location: {}. Current: {}°{}, {}",
            data.location, data.current.temp, data.unit, data.current.condition
        );
        if let Some(h) = data.current.humidity {
            ctx.push_str(&format!(", humidity {h:.0}%"));
        }
        if let Some(w) = data.current.wind_speed {
            ctx.push_str(&format!(", wind {w:.0} m/s"));
        }
        ctx.push('.');

        if !data.forecast.is_empty() {
            ctx.push_str(" Forecast:");
            for day in &data.forecast {
                ctx.push_str(&format!(
                    " {} {}/{}°{} {}.",
                    day.day, day.temp_high, day.temp_low, data.unit, day.condition
                ));
            }
        }

        ctx
    }

    /// Generate a simple template answer when AI is not available.
    fn fallback_answer(data: &super::weather::WeatherData, question: &str) -> String {
        let q = question.to_lowercase();
        let has_rain = data.current.condition.contains("rain")
            || data.forecast.iter().any(|d| d.condition.contains("rain"));

        if q.contains("rain") || q.contains("umbrella") {
            if has_rain {
                format!(
                    "Yes, {} is expected in {}. You might want an umbrella.",
                    data.current.condition, data.location
                )
            } else {
                format!(
                    "No rain expected in {}. Currently {}°{}, {}.",
                    data.location, data.current.temp, data.unit, data.current.condition
                )
            }
        } else if q.contains("cold")
            || q.contains("warm")
            || q.contains("hot")
            || q.contains("jacket")
        {
            format!(
                "It's currently {}°{} in {} with {}.",
                data.current.temp, data.unit, data.location, data.current.condition
            )
        } else {
            format!(
                "Currently {}°{} in {}, {}.",
                data.current.temp, data.unit, data.location, data.current.condition
            )
        }
    }
}

#[async_trait]
impl ActionHandler for WeatherAskHandler {
    fn id(&self) -> &str {
        "weather-ask"
    }

    fn description(&self) -> &str {
        "Answer conversational weather questions using real weather data"
    }

    fn default_risk(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let question = args.trim();
        if question.is_empty() {
            return Ok(ActionResult::err("No question provided".to_string()));
        }

        let start = Instant::now();

        // Fetch real weather data (location auto-detected or empty = default)
        let weather_data = self.weather.get_weather_data("").await?;
        let context = Self::format_context(&weather_data);

        let report_url = format!(
            "https://www.accuweather.com/en/search-locations?query={}",
            urlencoding::encode(&weather_data.location)
        );

        let answer = if let Some(provider) = &self.ai_provider {
            let prompt = format!("{context}\n\nUser question: {question}");
            match tokio::time::timeout(
                WEATHER_ASK_TIMEOUT,
                provider.answer_question(WEATHER_ASK_SYSTEM_PROMPT, &prompt),
            )
            .await
            {
                Ok(Ok(text)) => text,
                Ok(Err(e)) => {
                    tracing::warn!("Weather-ask AI failed: {e}, using fallback");
                    Self::fallback_answer(&weather_data, question)
                }
                Err(_) => {
                    tracing::warn!("Weather-ask AI timed out, using fallback");
                    Self::fallback_answer(&weather_data, question)
                }
            }
        } else {
            Self::fallback_answer(&weather_data, question)
        };

        let duration_ms = start.elapsed().as_millis() as u64;

        Ok(ActionResult::ok(answer, OutputType::Text)
            .with_link(report_url)
            .with_duration(duration_ms))
    }
}
