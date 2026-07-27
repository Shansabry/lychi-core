// Currency exchange rates provided by ExchangeRate-API (https://www.exchangerate-api.com)
// Free tier, no API key required. Rates cached locally for 10 minutes.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use async_trait::async_trait;

use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, OutputType,
};
use crate::error::LychiError;

pub struct CalcHandler;

impl Default for CalcHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl CalcHandler {
    pub fn new() -> Self {
        Self
    }

    fn evaluate(expr: &str) -> Option<f64> {
        meval::eval_str(expr).ok()
    }

    fn format_result(value: f64) -> String {
        if value.fract() == 0.0 && value.abs() < 1e15 {
            format!("{}", value as i64)
        } else {
            let s = format!("{:.10}", value);
            let s = s.trim_end_matches('0');
            let s = s.trim_end_matches('.');
            s.to_string()
        }
    }

    /// Try to parse and evaluate a unit/currency conversion expression.
    /// Returns (display_label, raw_value) on success.
    fn try_conversion(input: &str) -> Option<(String, String)> {
        let conv = parse_conversion(input)?;

        match conv {
            Conversion::Unit { value, from, to } => {
                let result = convert_unit(value, &from, &to)?;
                let formatted = Self::format_result(result);
                let to_display = unit_display_name(&to);
                Some((
                    format!("= {} {}", formatted, to_display),
                    format!("{} {}", formatted, to_display),
                ))
            }
            Conversion::Currency { value, from, to } => {
                let (result, rate) = convert_currency(value, &from, &to)?;
                let formatted = Self::format_result(result);
                let from_upper = from.to_uppercase();
                let to_upper = to.to_uppercase();
                Some((
                    format!(
                        "= {} {} (1 {} = {} {})",
                        formatted,
                        to_upper,
                        from_upper,
                        Self::format_result(rate),
                        to_upper
                    ),
                    format!("{} {}", formatted, to_upper),
                ))
            }
        }
    }
}

#[async_trait]
impl ActionHandler for CalcHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["calc"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "calc"
    }

    fn description(&self) -> &str {
        "Evaluate math expressions, unit conversions, and currency conversions"
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Utilities
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let expr = args.trim();
        if expr.is_empty() {
            return Ok(ActionResult::err(
                "Usage: calc <expression>, =<expression>, or <number> <unit> to <unit>".to_string(),
            ));
        }

        // A currency conversion needs fresh rates. The startup fetch expires
        // after the TTL, so refresh on demand here (execute is async) before
        // converting — otherwise a stale cache would fail as "Invalid
        // expression" even though the input is perfectly valid.
        if matches!(parse_conversion(expr), Some(Conversion::Currency { .. })) && !rates_are_fresh()
        {
            fetch_exchange_rates().await;
            if !rates_are_fresh() {
                return Ok(ActionResult::err(
                    "Couldn't fetch exchange rates — check your connection".to_string(),
                ));
            }
        }

        // Try conversion first (unit/currency)
        if let Some((_label, raw_value)) = Self::try_conversion(expr) {
            return Ok(ActionResult::ok(raw_value, OutputType::Status));
        }

        // Fall back to math evaluation
        match Self::evaluate(expr) {
            Some(result) => Ok(ActionResult::ok(
                Self::format_result(result),
                OutputType::Status,
            )),
            None => Ok(ActionResult::err(format!("Invalid expression: {expr}"))),
        }
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let expr = partial.trim();
        if expr.is_empty() {
            return Vec::new();
        }

        // If the user is typing a currency conversion but rates are stale, warm
        // them in the background (fire-and-forget) so the live preview appears
        // on a later keystroke and Enter is instant. Non-blocking — this pass
        // still returns whatever it can compute now.
        if matches!(parse_conversion(expr), Some(Conversion::Currency { .. })) && !rates_are_fresh()
        {
            tokio::spawn(fetch_exchange_rates());
        }

        // Try conversion first. Selecting the result re-evaluates via the calc
        // handler (which shows the value in a result card, copyable).
        if let Some((label, _raw)) = Self::try_conversion(expr) {
            return vec![CompletionItem {
                label,
                icon_path: None,
                score: 1000,
                description: None,
                reason: None,
                thumb_b64: None,
                run: Some(format!("calc {expr}")),
                // An answer, not a command — the frontend shows it instead of
                // running it, and must not parse that intent out of the label.
                kind: Some(crate::action_registry::CompletionKind::Calc),
                ..Default::default()
            }];
        }

        // Fall back to math evaluation
        if let Some(result) = Self::evaluate(expr) {
            vec![CompletionItem {
                label: format!("= {}", Self::format_result(result)),
                icon_path: None,
                score: 1000,
                description: None,
                reason: None,
                thumb_b64: None,
                run: Some(format!("calc {expr}")),
                // An answer, not a command — the frontend shows it instead of
                // running it, and must not parse that intent out of the label.
                kind: Some(crate::action_registry::CompletionKind::Calc),
                ..Default::default()
            }]
        } else {
            Vec::new()
        }
    }
}

// ---------------------------------------------------------------------------
// Conversion parsing
// ---------------------------------------------------------------------------

enum Conversion {
    Unit {
        value: f64,
        from: String,
        to: String,
    },
    Currency {
        value: f64,
        from: String,
        to: String,
    },
}

/// Parse "<number> <unit> to/in <unit>" patterns.
/// Also handles "<number><unit> to <unit>" (no space between number and unit).
fn parse_conversion(input: &str) -> Option<Conversion> {
    let lower = input.to_lowercase();
    let lower = lower.trim();

    // Find "to" or "in" separator
    let (left, right) = if let Some(pos) = lower.find(" to ") {
        (&lower[..pos], lower[pos + 4..].trim())
    } else {
        let pos = lower.find(" in ")?;
        (&lower[..pos], lower[pos + 4..].trim())
    };

    let left = left.trim();
    if left.is_empty() || right.is_empty() {
        return None;
    }

    // Split left into number + unit
    // Try: "100 usd", "100usd", "5.5 kg", "5.5kg"
    let (value, from_unit) = split_number_unit(left)?;

    let to_unit = right.trim().to_string();

    // Determine if this is a currency or unit conversion
    if is_currency_code(&from_unit) && is_currency_code(&to_unit) {
        Some(Conversion::Currency {
            value,
            from: from_unit,
            to: to_unit,
        })
    } else if resolve_unit(&from_unit).is_some() && resolve_unit(&to_unit).is_some() {
        Some(Conversion::Unit {
            value,
            from: from_unit,
            to: to_unit,
        })
    } else {
        None
    }
}

/// Split "100 usd" or "100usd" into (100.0, "usd").
fn split_number_unit(s: &str) -> Option<(f64, String)> {
    // Try space-separated first: "100 usd"
    if let Some(space_pos) = s.find(' ') {
        let num_str = s[..space_pos].trim();
        let unit_str = s[space_pos + 1..].trim();
        if let Ok(value) = num_str.parse::<f64>()
            && !unit_str.is_empty()
        {
            return Some((value, unit_str.to_string()));
        }
    }

    // Try no-space: "100usd", "5.5kg"
    // Find where digits end and letters begin
    let mut split_at = 0;
    for (i, ch) in s.char_indices() {
        if ch.is_ascii_digit() || ch == '.' || ch == '-' {
            split_at = i + ch.len_utf8();
        } else {
            break;
        }
    }
    if split_at > 0 && split_at < s.len() {
        let num_str = &s[..split_at];
        let unit_str = s[split_at..].trim();
        if let Ok(value) = num_str.parse::<f64>()
            && !unit_str.is_empty()
        {
            return Some((value, unit_str.to_string()));
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Unit conversion
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
enum UnitCategory {
    Length,
    Weight,
    Temperature,
    Volume,
    Data,
    Area,
    Speed,
    Time,
    Pressure,
    Energy,
    Angle,
}

#[derive(Debug, Clone, Copy)]
struct UnitInfo {
    category: UnitCategory,
    /// Factor to convert to the base unit of this category.
    /// For temperature, this is unused (special-cased).
    to_base: f64,
}

/// Normalize unit aliases to canonical names and return unit info.
fn resolve_unit(name: &str) -> Option<(&'static str, UnitInfo)> {
    let canonical = match name {
        // Length (base: meters)
        "km" | "kilometer" | "kilometers" | "kilometres" => "km",
        "mi" | "mile" | "miles" => "mi",
        "m" | "meter" | "meters" | "metres" => "m",
        "ft" | "foot" | "feet" => "ft",
        "cm" | "centimeter" | "centimeters" | "centimetres" => "cm",
        "in" | "inch" | "inches" => "in",
        "mm" | "millimeter" | "millimeters" | "millimetres" => "mm",
        "yd" | "yard" | "yards" => "yd",

        // Weight (base: grams)
        "kg" | "kilogram" | "kilograms" => "kg",
        "lb" | "lbs" | "pound" | "pounds" => "lb",
        "g" | "gram" | "grams" => "g",
        "oz" | "ounce" | "ounces" => "oz",
        "mg" | "milligram" | "milligrams" => "mg",
        "ton" | "tons" | "tonne" | "tonnes" => "ton",

        // Temperature (special-cased)
        "c" | "°c" | "celsius" => "c",
        "f" | "°f" | "fahrenheit" => "f",
        "k" | "kelvin" => "k",

        // Volume (base: liters)
        "l" | "liter" | "liters" | "litre" | "litres" => "l",
        "ml" | "milliliter" | "milliliters" | "millilitres" => "ml",
        "gal" | "gallon" | "gallons" => "gal",
        "qt" | "quart" | "quarts" => "qt",
        "pt" | "pint" | "pints" => "pt",
        "cup" | "cups" => "cup",
        "fl oz" | "floz" => "floz",
        "tbsp" | "tablespoon" | "tablespoons" => "tbsp",
        "tsp" | "teaspoon" | "teaspoons" => "tsp",

        // Data (base: bytes)
        "b" | "byte" | "bytes" => "b",
        "kb" | "kilobyte" | "kilobytes" => "kb",
        "mb" | "megabyte" | "megabytes" => "mb",
        "gb" | "gigabyte" | "gigabytes" => "gb",
        "tb" | "terabyte" | "terabytes" => "tb",
        "pb" | "petabyte" | "petabytes" => "pb",
        "kib" | "kibibyte" | "kibibytes" => "kib",
        "mib" | "mebibyte" | "mebibytes" => "mib",
        "gib" | "gibibyte" | "gibibytes" => "gib",
        "tib" | "tebibyte" | "tebibytes" => "tib",

        // Area (base: square meters)
        "sq m" | "sqm" | "m2" | "m²" | "square meter" | "square meters" | "square metres" => "sqm",
        "sq ft" | "sqft" | "ft2" | "ft²" | "square foot" | "square feet" => "sqft",
        "sq km" | "sqkm" | "km2" | "km²" | "square kilometer" | "square kilometers"
        | "square kilometres" => "sqkm",
        "sq mi" | "sqmi" | "mi2" | "mi²" | "square mile" | "square miles" => "sqmi",
        "sq in" | "sqin" | "in2" | "in²" | "square inch" | "square inches" => "sqin",
        "sq yd" | "sqyd" | "yd2" | "yd²" | "square yard" | "square yards" => "sqyd",
        "acre" | "acres" | "ac" => "acre",
        "hectare" | "hectares" | "ha" => "ha",
        "are" | "ares" => "are",
        "cent" | "cents" => "cent",
        "guntha" | "gunthas" => "guntha",
        "ground" | "grounds" => "ground",
        "kanal" | "kanals" => "kanal",
        "marla" | "marlas" => "marla",
        "bigha" | "bighas" => "bigha",

        // Speed (base: meters per second)
        "m/s" | "mps" | "meters/s" => "m/s",
        "km/h" | "kmh" | "kph" | "kmph" => "km/h",
        "mph" | "mi/h" => "mph",
        "knot" | "knots" | "kn" | "kt" => "knot",
        "ft/s" | "fps" | "feet/s" => "ft/s",

        // Time (base: seconds)
        "s" | "sec" | "second" | "seconds" | "secs" => "s",
        "ms" | "millisecond" | "milliseconds" => "ms",
        "us" | "µs" | "microsecond" | "microseconds" => "us",
        "ns" | "nanosecond" | "nanoseconds" => "ns",
        "min" | "minute" | "minutes" | "mins" => "min",
        "h" | "hr" | "hour" | "hours" | "hrs" => "hr",
        "day" | "days" => "day",
        "week" | "weeks" | "wk" | "wks" => "week",
        "month" | "months" | "mo" => "month",
        "year" | "years" | "yr" | "yrs" => "year",

        // Pressure (base: pascals)
        "pa" | "pascal" | "pascals" => "pa",
        "kpa" | "kilopascal" | "kilopascals" => "kpa",
        "mpa" | "megapascal" | "megapascals" => "mpa",
        "bar" | "bars" => "bar",
        "mbar" | "millibar" | "millibars" => "mbar",
        "atm" | "atmosphere" | "atmospheres" => "atm",
        "psi" => "psi",
        "mmhg" | "torr" => "mmhg",
        "inhg" => "inhg",

        // Energy (base: joules)
        "j" | "joule" | "joules" => "j",
        "kj" | "kilojoule" | "kilojoules" => "kj",
        "cal" | "calorie" | "calories" => "cal",
        "kcal" | "kilocalorie" | "kilocalories" => "kcal",
        "kwh" | "kilowatt-hour" | "kilowatt-hours" => "kwh",
        "wh" | "watt-hour" | "watt-hours" => "wh",
        "btu" | "btus" => "btu",
        "ev" | "electronvolt" | "electronvolts" => "ev",

        // Angle (base: degrees)
        "deg" | "degree" | "degrees" | "°" => "deg",
        "rad" | "radian" | "radians" => "rad",
        "grad" | "gradian" | "gradians" | "gon" => "grad",
        "turn" | "turns" | "rev" | "revolution" | "revolutions" => "turn",

        _ => return None,
    };

    let info = match canonical {
        // Length → meters
        "km" => UnitInfo {
            category: UnitCategory::Length,
            to_base: 1000.0,
        },
        "mi" => UnitInfo {
            category: UnitCategory::Length,
            to_base: 1609.344,
        },
        "m" => UnitInfo {
            category: UnitCategory::Length,
            to_base: 1.0,
        },
        "ft" => UnitInfo {
            category: UnitCategory::Length,
            to_base: 0.3048,
        },
        "cm" => UnitInfo {
            category: UnitCategory::Length,
            to_base: 0.01,
        },
        "in" => UnitInfo {
            category: UnitCategory::Length,
            to_base: 0.0254,
        },
        "mm" => UnitInfo {
            category: UnitCategory::Length,
            to_base: 0.001,
        },
        "yd" => UnitInfo {
            category: UnitCategory::Length,
            to_base: 0.9144,
        },

        // Weight → grams
        "kg" => UnitInfo {
            category: UnitCategory::Weight,
            to_base: 1000.0,
        },
        "lb" => UnitInfo {
            category: UnitCategory::Weight,
            to_base: 453.592,
        },
        "g" => UnitInfo {
            category: UnitCategory::Weight,
            to_base: 1.0,
        },
        "oz" => UnitInfo {
            category: UnitCategory::Weight,
            to_base: 28.3495,
        },
        "mg" => UnitInfo {
            category: UnitCategory::Weight,
            to_base: 0.001,
        },
        "ton" => UnitInfo {
            category: UnitCategory::Weight,
            to_base: 1_000_000.0,
        },

        // Temperature — to_base unused, special-cased
        "c" | "f" | "k" => UnitInfo {
            category: UnitCategory::Temperature,
            to_base: 0.0,
        },

        // Volume → liters
        "l" => UnitInfo {
            category: UnitCategory::Volume,
            to_base: 1.0,
        },
        "ml" => UnitInfo {
            category: UnitCategory::Volume,
            to_base: 0.001,
        },
        "gal" => UnitInfo {
            category: UnitCategory::Volume,
            to_base: 3.78541,
        },
        "qt" => UnitInfo {
            category: UnitCategory::Volume,
            to_base: 0.946353,
        },
        "pt" => UnitInfo {
            category: UnitCategory::Volume,
            to_base: 0.473176,
        },
        "cup" => UnitInfo {
            category: UnitCategory::Volume,
            to_base: 0.236588,
        },
        "floz" => UnitInfo {
            category: UnitCategory::Volume,
            to_base: 0.0295735,
        },
        "tbsp" => UnitInfo {
            category: UnitCategory::Volume,
            to_base: 0.0147868,
        },
        "tsp" => UnitInfo {
            category: UnitCategory::Volume,
            to_base: 0.00492892,
        },

        // Data → bytes
        "b" => UnitInfo {
            category: UnitCategory::Data,
            to_base: 1.0,
        },
        "kb" => UnitInfo {
            category: UnitCategory::Data,
            to_base: 1000.0,
        },
        "mb" => UnitInfo {
            category: UnitCategory::Data,
            to_base: 1_000_000.0,
        },
        "gb" => UnitInfo {
            category: UnitCategory::Data,
            to_base: 1_000_000_000.0,
        },
        "tb" => UnitInfo {
            category: UnitCategory::Data,
            to_base: 1_000_000_000_000.0,
        },
        "pb" => UnitInfo {
            category: UnitCategory::Data,
            to_base: 1_000_000_000_000_000.0,
        },
        "kib" => UnitInfo {
            category: UnitCategory::Data,
            to_base: 1024.0,
        },
        "mib" => UnitInfo {
            category: UnitCategory::Data,
            to_base: 1_048_576.0,
        },
        "gib" => UnitInfo {
            category: UnitCategory::Data,
            to_base: 1_073_741_824.0,
        },
        "tib" => UnitInfo {
            category: UnitCategory::Data,
            to_base: 1_099_511_627_776.0,
        },

        // Area → square meters
        "sqm" => UnitInfo {
            category: UnitCategory::Area,
            to_base: 1.0,
        },
        "sqft" => UnitInfo {
            category: UnitCategory::Area,
            to_base: 0.092903,
        },
        "sqkm" => UnitInfo {
            category: UnitCategory::Area,
            to_base: 1_000_000.0,
        },
        "sqmi" => UnitInfo {
            category: UnitCategory::Area,
            to_base: 2_589_988.0,
        },
        "sqin" => UnitInfo {
            category: UnitCategory::Area,
            to_base: 0.00064516,
        },
        "sqyd" => UnitInfo {
            category: UnitCategory::Area,
            to_base: 0.836127,
        },
        "acre" => UnitInfo {
            category: UnitCategory::Area,
            to_base: 4046.86,
        },
        "ha" => UnitInfo {
            category: UnitCategory::Area,
            to_base: 10_000.0,
        },
        "are" => UnitInfo {
            category: UnitCategory::Area,
            to_base: 100.0,
        },
        "cent" => UnitInfo {
            category: UnitCategory::Area,
            to_base: 40.4686,
        }, // 1/100 acre
        "guntha" => UnitInfo {
            category: UnitCategory::Area,
            to_base: 101.17,
        }, // 1089 sq ft
        "ground" => UnitInfo {
            category: UnitCategory::Area,
            to_base: 222.967,
        }, // 2400 sq ft (South India)
        "kanal" => UnitInfo {
            category: UnitCategory::Area,
            to_base: 505.857,
        }, // 5445 sq ft (North India/Pakistan)
        "marla" => UnitInfo {
            category: UnitCategory::Area,
            to_base: 25.2929,
        }, // 1/20 kanal
        "bigha" => UnitInfo {
            category: UnitCategory::Area,
            to_base: 2529.29,
        }, // varies by region, using standard bigha

        // Speed → meters per second
        "m/s" => UnitInfo {
            category: UnitCategory::Speed,
            to_base: 1.0,
        },
        "km/h" => UnitInfo {
            category: UnitCategory::Speed,
            to_base: 0.277778,
        },
        "mph" => UnitInfo {
            category: UnitCategory::Speed,
            to_base: 0.44704,
        },
        "knot" => UnitInfo {
            category: UnitCategory::Speed,
            to_base: 0.514444,
        },
        "ft/s" => UnitInfo {
            category: UnitCategory::Speed,
            to_base: 0.3048,
        },

        // Time → seconds
        "s" => UnitInfo {
            category: UnitCategory::Time,
            to_base: 1.0,
        },
        "ms" => UnitInfo {
            category: UnitCategory::Time,
            to_base: 0.001,
        },
        "us" => UnitInfo {
            category: UnitCategory::Time,
            to_base: 0.000001,
        },
        "ns" => UnitInfo {
            category: UnitCategory::Time,
            to_base: 0.000000001,
        },
        "min" => UnitInfo {
            category: UnitCategory::Time,
            to_base: 60.0,
        },
        "hr" => UnitInfo {
            category: UnitCategory::Time,
            to_base: 3600.0,
        },
        "day" => UnitInfo {
            category: UnitCategory::Time,
            to_base: 86400.0,
        },
        "week" => UnitInfo {
            category: UnitCategory::Time,
            to_base: 604800.0,
        },
        "month" => UnitInfo {
            category: UnitCategory::Time,
            to_base: 2_592_000.0,
        }, // 30 days
        "year" => UnitInfo {
            category: UnitCategory::Time,
            to_base: 31_557_600.0,
        }, // 365.25 days

        // Pressure → pascals
        "pa" => UnitInfo {
            category: UnitCategory::Pressure,
            to_base: 1.0,
        },
        "kpa" => UnitInfo {
            category: UnitCategory::Pressure,
            to_base: 1000.0,
        },
        "mpa" => UnitInfo {
            category: UnitCategory::Pressure,
            to_base: 1_000_000.0,
        },
        "bar" => UnitInfo {
            category: UnitCategory::Pressure,
            to_base: 100_000.0,
        },
        "mbar" => UnitInfo {
            category: UnitCategory::Pressure,
            to_base: 100.0,
        },
        "atm" => UnitInfo {
            category: UnitCategory::Pressure,
            to_base: 101_325.0,
        },
        "psi" => UnitInfo {
            category: UnitCategory::Pressure,
            to_base: 6894.76,
        },
        "mmhg" => UnitInfo {
            category: UnitCategory::Pressure,
            to_base: 133.322,
        },
        "inhg" => UnitInfo {
            category: UnitCategory::Pressure,
            to_base: 3386.39,
        },

        // Energy → joules
        "j" => UnitInfo {
            category: UnitCategory::Energy,
            to_base: 1.0,
        },
        "kj" => UnitInfo {
            category: UnitCategory::Energy,
            to_base: 1000.0,
        },
        "cal" => UnitInfo {
            category: UnitCategory::Energy,
            to_base: 4.184,
        },
        "kcal" => UnitInfo {
            category: UnitCategory::Energy,
            to_base: 4184.0,
        },
        "kwh" => UnitInfo {
            category: UnitCategory::Energy,
            to_base: 3_600_000.0,
        },
        "wh" => UnitInfo {
            category: UnitCategory::Energy,
            to_base: 3600.0,
        },
        "btu" => UnitInfo {
            category: UnitCategory::Energy,
            to_base: 1055.06,
        },
        "ev" => UnitInfo {
            category: UnitCategory::Energy,
            to_base: 1.602e-19,
        },

        // Angle → degrees
        "deg" => UnitInfo {
            category: UnitCategory::Angle,
            to_base: 1.0,
        },
        "rad" => UnitInfo {
            category: UnitCategory::Angle,
            to_base: 57.2958,
        }, // 180/π
        "grad" => UnitInfo {
            category: UnitCategory::Angle,
            to_base: 0.9,
        },
        "turn" => UnitInfo {
            category: UnitCategory::Angle,
            to_base: 360.0,
        },

        _ => return None,
    };

    Some((canonical, info))
}

fn unit_display_name(unit: &str) -> &'static str {
    match resolve_unit(unit) {
        Some((canonical, _)) => canonical,
        None => "?",
    }
}

fn convert_unit(value: f64, from: &str, to: &str) -> Option<f64> {
    let (from_canonical, from_info) = resolve_unit(from)?;
    let (to_canonical, to_info) = resolve_unit(to)?;

    if from_info.category != to_info.category {
        return None; // Can't convert kg to meters
    }

    // Temperature is special
    if from_info.category == UnitCategory::Temperature {
        return convert_temperature(value, from_canonical, to_canonical);
    }

    // Standard: value * from_to_base / to_to_base
    Some(value * from_info.to_base / to_info.to_base)
}

fn convert_temperature(value: f64, from: &str, to: &str) -> Option<f64> {
    // Convert to Celsius first, then to target
    let celsius = match from {
        "c" => value,
        "f" => (value - 32.0) * 5.0 / 9.0,
        "k" => value - 273.15,
        _ => return None,
    };

    let result = match to {
        "c" => celsius,
        "f" => celsius * 9.0 / 5.0 + 32.0,
        "k" => celsius + 273.15,
        _ => return None,
    };

    Some(result)
}

// ---------------------------------------------------------------------------
// Currency conversion
// ---------------------------------------------------------------------------

const CURRENCY_CODES: &[&str] = &[
    "usd", "eur", "gbp", "jpy", "cny", "inr", "aud", "cad", "chf", "hkd", "sgd", "sek", "nok",
    "dkk", "nzd", "zar", "krw", "thb", "myr", "php", "idr", "brl", "mxn", "rub", "try", "pln",
    "czk", "huf", "ron", "bgn", "hrk", "isk", "aed", "sar", "qar", "kwd", "bhd", "omr", "egp",
    "ngn", "kes", "ghs", "tzs", "ugx", "cop", "ars", "clp", "pen", "vnd", "twd", "pkr", "bdt",
    "lkr",
];

fn is_currency_code(code: &str) -> bool {
    CURRENCY_CODES.contains(&code.to_lowercase().as_str())
}

/// Cached exchange rates with TTL.
struct RateCache {
    rates: HashMap<String, f64>,
    fetched_at: Instant,
    base: String,
}

static RATE_CACHE: Mutex<Option<RateCache>> = Mutex::new(None);

const RATE_CACHE_TTL_SECS: u64 = 600; // 10 minutes

/// Whether exchange rates are cached and within their TTL. When false, the
/// caller should refresh via `fetch_exchange_rates()` before converting.
fn rates_are_fresh() -> bool {
    RATE_CACHE
        .lock()
        .ok()
        .and_then(|c| {
            c.as_ref()
                .map(|c| c.fetched_at.elapsed().as_secs() <= RATE_CACHE_TTL_SECS)
        })
        .unwrap_or(false)
}

fn convert_currency(value: f64, from: &str, to: &str) -> Option<(f64, f64)> {
    let from_upper = from.to_uppercase();
    let to_upper = to.to_uppercase();

    let cache = RATE_CACHE.lock().ok()?;
    let cache = cache.as_ref()?;

    // Check if cache is still fresh
    if cache.fetched_at.elapsed().as_secs() > RATE_CACHE_TTL_SECS {
        return None;
    }

    // Rates are relative to cache.base (usually USD)
    let from_rate = if from_upper == cache.base {
        1.0
    } else {
        *cache.rates.get(&from_upper)?
    };

    let to_rate = if to_upper == cache.base {
        1.0
    } else {
        *cache.rates.get(&to_upper)?
    };

    // Convert: value in FROM → base → TO
    let rate = to_rate / from_rate;
    let result = value * rate;

    Some((result, rate))
}

/// Fetch exchange rates in the background. Call this at startup or on first currency query.
pub async fn fetch_exchange_rates() {
    let url = "https://open.er-api.com/v6/latest/USD";

    let resp = match reqwest::get(url).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Failed to fetch exchange rates: {e}");
            return;
        }
    };

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("Failed to parse exchange rates: {e}");
            return;
        }
    };

    if let Some(rates_obj) = body.get("rates").and_then(|r| r.as_object()) {
        let mut rates = HashMap::with_capacity(rates_obj.len());
        for (code, val) in rates_obj {
            if let Some(rate) = val.as_f64() {
                rates.insert(code.clone(), rate);
            }
        }

        let count = rates.len();
        if let Ok(mut cache) = RATE_CACHE.lock() {
            *cache = Some(RateCache {
                rates,
                fetched_at: Instant::now(),
                base: "USD".to_string(),
            });
        }
        tracing::info!("Exchange rates cached: {count} currencies");
    }
}

/// Check if conversion expression pattern is present (for pattern routing).
/// Pattern: `<number> <unit/currency> to/in <unit/currency>`
pub fn is_conversion_expression(input: &str) -> bool {
    parse_conversion(&input.to_lowercase()).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    /// Serializes tests that mutate the shared RATE_CACHE static.
    static RATE_TEST_LOCK: StdMutex<()> = StdMutex::new(());

    #[test]
    fn test_math_evaluation() {
        assert_eq!(CalcHandler::evaluate("2+2"), Some(4.0));
        assert_eq!(CalcHandler::evaluate("sqrt(144)"), Some(12.0));
        assert!(CalcHandler::evaluate("invalid").is_none());
    }

    #[test]
    fn currency_conversion_uses_cached_rates() {
        let _g = RATE_TEST_LOCK.lock().unwrap();
        // Seed a fresh cache (base USD) and verify conversion + freshness.
        {
            let mut cache = RATE_CACHE.lock().unwrap();
            let mut rates = HashMap::new();
            rates.insert("INR".to_string(), 80.0);
            rates.insert("EUR".to_string(), 0.9);
            *cache = Some(RateCache {
                rates,
                fetched_at: Instant::now(),
                base: "USD".to_string(),
            });
        }
        assert!(rates_are_fresh());
        let (result, rate) = convert_currency(100.0, "usd", "inr").unwrap();
        assert!((rate - 80.0).abs() < 1e-9);
        assert!((result - 8000.0).abs() < 1e-6);
        // Cross-rate through the USD base: 100 EUR → USD → INR.
        let (eur_inr, _) = convert_currency(100.0, "eur", "inr").unwrap();
        assert!((eur_inr - 100.0 * (80.0 / 0.9)).abs() < 1e-6);
        *RATE_CACHE.lock().unwrap() = None;
    }

    #[test]
    fn stale_cache_is_not_fresh() {
        let _g = RATE_TEST_LOCK.lock().unwrap();
        {
            let mut cache = RATE_CACHE.lock().unwrap();
            *cache = Some(RateCache {
                rates: HashMap::new(),
                fetched_at: Instant::now() - Duration::from_secs(RATE_CACHE_TTL_SECS + 60),
                base: "USD".to_string(),
            });
        }
        assert!(!rates_are_fresh());
        // A stale cache yields no conversion (caller must refresh first).
        assert!(convert_currency(1.0, "usd", "eur").is_none());
        // Clean up so other tests aren't affected by the stale entry.
        *RATE_CACHE.lock().unwrap() = None;
    }

    #[test]
    fn test_format_result() {
        assert_eq!(CalcHandler::format_result(4.0), "4");
        assert_eq!(CalcHandler::format_result(3.14159), "3.14159");
        assert_eq!(CalcHandler::format_result(100.0), "100");
    }

    #[test]
    fn test_split_number_unit() {
        assert_eq!(
            split_number_unit("100 usd"),
            Some((100.0, "usd".to_string()))
        );
        assert_eq!(split_number_unit("5.5kg"), Some((5.5, "kg".to_string())));
        assert_eq!(split_number_unit("72 f"), Some((72.0, "f".to_string())));
        assert!(split_number_unit("abc").is_none());
    }

    #[test]
    fn test_unit_conversion() {
        // Length
        let r = convert_unit(1.0, "km", "mi").unwrap();
        assert!((r - 0.621371).abs() < 0.001);

        let r = convert_unit(1.0, "ft", "cm").unwrap();
        assert!((r - 30.48).abs() < 0.01);

        // Weight
        let r = convert_unit(1.0, "kg", "lb").unwrap();
        assert!((r - 2.20462).abs() < 0.01);

        // Temperature
        let r = convert_unit(100.0, "c", "f").unwrap();
        assert!((r - 212.0).abs() < 0.01);

        let r = convert_unit(72.0, "f", "c").unwrap();
        assert!((r - 22.2222).abs() < 0.01);

        let r = convert_unit(0.0, "c", "k").unwrap();
        assert!((r - 273.15).abs() < 0.01);

        // Volume
        let r = convert_unit(1.0, "gal", "l").unwrap();
        assert!((r - 3.78541).abs() < 0.01);

        // Data
        let r = convert_unit(1.0, "gb", "mb").unwrap();
        assert!((r - 1000.0).abs() < 0.01);

        let r = convert_unit(1.0, "gib", "mib").unwrap();
        assert!((r - 1024.0).abs() < 0.01);

        // Area
        let r = convert_unit(1.0, "acre", "sqft").unwrap();
        assert!((r - 43560.0).abs() < 10.0);

        let r = convert_unit(1.0, "ha", "acre").unwrap();
        assert!((r - 2.47105).abs() < 0.01);

        let r = convert_unit(100.0, "cent", "acre").unwrap();
        assert!((r - 1.0).abs() < 0.001);

        let r = convert_unit(1.0, "cent", "sqft").unwrap();
        assert!((r - 435.6).abs() < 1.0);

        let r = convert_unit(1.0, "kanal", "sqft").unwrap();
        assert!((r - 5445.0).abs() < 10.0);

        let r = convert_unit(20.0, "marla", "kanal").unwrap();
        assert!((r - 1.0).abs() < 0.01);

        // Speed
        let r = convert_unit(100.0, "km/h", "mph").unwrap();
        assert!((r - 62.137).abs() < 0.1);

        let r = convert_unit(1.0, "knot", "km/h").unwrap();
        assert!((r - 1.852).abs() < 0.01);

        // Time
        let r = convert_unit(1.0, "hr", "min").unwrap();
        assert!((r - 60.0).abs() < 0.01);

        let r = convert_unit(1.0, "day", "hr").unwrap();
        assert!((r - 24.0).abs() < 0.01);

        let r = convert_unit(1.0, "year", "day").unwrap();
        assert!((r - 365.25).abs() < 0.1);

        // Pressure
        let r = convert_unit(1.0, "atm", "psi").unwrap();
        assert!((r - 14.696).abs() < 0.01);

        let r = convert_unit(1.0, "bar", "kpa").unwrap();
        assert!((r - 100.0).abs() < 0.01);

        // Energy
        let r = convert_unit(1.0, "kcal", "kj").unwrap();
        assert!((r - 4.184).abs() < 0.01);

        let r = convert_unit(1.0, "kwh", "kj").unwrap();
        assert!((r - 3600.0).abs() < 1.0);

        // Angle
        let r = convert_unit(180.0, "deg", "rad").unwrap();
        assert!((r - std::f64::consts::PI).abs() < 0.001);

        let r = convert_unit(1.0, "turn", "deg").unwrap();
        assert!((r - 360.0).abs() < 0.01);

        // Cross-category should fail
        assert!(convert_unit(1.0, "kg", "m").is_none());
    }

    #[test]
    fn test_parse_conversion() {
        // Unit conversion
        assert!(parse_conversion("5 kg to lb").is_some());
        assert!(parse_conversion("100cm to inches").is_some());
        assert!(parse_conversion("72 f to c").is_some());

        // Currency conversion
        assert!(parse_conversion("250 usd to eur").is_some());
        assert!(parse_conversion("100 EUR in INR").is_some());

        // Invalid
        assert!(parse_conversion("hello world").is_none());
        assert!(parse_conversion("5 kg").is_none()); // No "to"
    }

    #[test]
    fn test_is_conversion_expression() {
        assert!(is_conversion_expression("5 kg to lb"));
        assert!(is_conversion_expression("100 usd to eur"));
        assert!(is_conversion_expression("72°F to C"));
        assert!(!is_conversion_expression("2+2"));
        assert!(!is_conversion_expression("firefox"));
    }
}
