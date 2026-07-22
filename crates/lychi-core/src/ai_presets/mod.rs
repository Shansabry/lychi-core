//! AI presets — user-defined saved prompt templates ("AI Commands", Phase 3).
//!
//! A preset pairs a typed `keyword` with a `template` prompt containing an
//! `{input}` placeholder. Typing `<keyword> <text>` seeds an AI conversation
//! with the template, `{input}` replaced by `<text>`. This re-homes the old
//! `translate` / `summarize` / `rewrite` handlers as data instead of code, and
//! lets users add their own — a direct clone of the snippets store.

pub mod store;

use serde::{Deserialize, Serialize};

/// The placeholder in a template that is replaced with the user's input.
pub const INPUT_PLACEHOLDER: &str = "{input}";

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct AiPresetItem {
    pub id: String,
    /// The typed shortcut that invokes this preset (e.g. "translate").
    pub keyword: String,
    /// A human-friendly display name.
    pub name: String,
    /// The prompt template; `{input}` is replaced with the user's text.
    pub template: String,
    pub created_at: u64,
    pub updated_at: u64,
}

impl AiPresetItem {
    /// Render the template for a given user input, substituting `{input}`. If the
    /// template has no placeholder, the input is appended on a new line (so a
    /// bare instruction like "Summarize this:" still receives the text).
    pub fn render(&self, input: &str) -> String {
        if self.template.contains(INPUT_PLACEHOLDER) {
            self.template.replace(INPUT_PLACEHOLDER, input)
        } else if input.is_empty() {
            self.template.clone()
        } else {
            format!("{}\n\n{input}", self.template)
        }
    }
}
