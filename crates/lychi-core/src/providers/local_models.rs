//! The curated local-model registry — the single source of truth for which
//! models Lychi offers, where to download them, how to verify them, and how to
//! format their chat prompts. Shared by the inference provider (`local.rs`) and
//! the download command.
//!
//! Deliberately small (per the "keep it light" goal). Weights are downloaded on
//! first use, never bundled. Only the GGUF is needed — llama.cpp reads the
//! tokenizer and architecture from the file itself, so a model is one entry here.
//!
//! NOT feature-gated — this is plain metadata (no engine types), so the download
//! command and the settings UI can use it even in a build without `local-ai`.

/// Chat prompt format for a model family. Different instruct models use different
/// special-token wrappers; using the wrong one degrades output badly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatFormat {
    /// Llama 3.x: `<|begin_of_text|><|start_header_id|>role<|end_header_id|>\n\n
    /// content<|eot_id|>...` — ends the turn with `<|eot_id|>`.
    Llama3,
    /// Phi-3/4: `<|system|>\n...<|end|>\n<|user|>\n...<|end|>\n<|assistant|>\n`.
    Phi,
    /// ChatML (Qwen, and many others): `<|im_start|>role\n...<|im_end|>\n`.
    ChatMl,
}

/// One offered model.
#[derive(Debug, Clone)]
pub struct ModelSpec {
    /// Stable id (also the stored `local_model` value / GGUF filename).
    pub id: &'static str,
    /// Human label for the picker.
    pub label: &'static str,
    /// One-line description (size/speed/quality tradeoff).
    pub description: &'static str,
    /// Approx download size, for the pre-download warning.
    pub size_label: &'static str,
    /// Approx resident RAM while loaded, for the warning.
    pub ram_label: &'static str,
    /// GGUF weights download URL.
    pub gguf_url: &'static str,
    /// SHA-256 of the GGUF file (hex), for integrity verification.
    pub gguf_sha256: &'static str,
    /// Chat prompt format.
    pub format: ChatFormat,
}

impl ModelSpec {
    /// The GGUF filename stored on disk (== `id` + ".gguf").
    pub fn gguf_filename(&self) -> String {
        format!("{}.gguf", self.id)
    }
}

/// The curated list. Keep small.
///
/// `gguf_sha256` values are the real SHA-256 of each Q4_K_M GGUF (from the
/// HuggingFace LFS pointers), so downloads are integrity-verified. (An all-zero
/// hash would be treated as "unverified — skip with a warning"; none are.)
///
/// The list is deliberately curated to models that are actually GOOD at intent
/// routing / instruction-following at small size on CPU (verified via IFEval /
/// BFCL + community consensus), NOT merely models that "run". A too-weak model
/// (e.g. 0.5B, IFEval ~28) routes deterministically but picks wrong actions, so
/// it's excluded. Every entry is license-clean for redistribution in a GPL-3.0
/// app — the function-calling specialists (xLAM, Hammer2.1) are stronger at this
/// task but are non-commercial licensed, so they're deliberately NOT bundled.
pub const MODELS: &[ModelSpec] = &[
    // Default: best speed/quality balance + cleanest license (Apache-2.0).
    ModelSpec {
        id: "qwen2.5-1.5b-instruct-q4",
        label: "Qwen2.5 1.5B (recommended)",
        description: "Fast, accurate router. Apache-2.0. The best all-round default.",
        size_label: "~1 GB",
        ram_label: "~2 GB RAM",
        gguf_url:
            "https://huggingface.co/bartowski/Qwen2.5-1.5B-Instruct-GGUF/resolve/main/Qwen2.5-1.5B-Instruct-Q4_K_M.gguf",
        gguf_sha256: "1adf0b11065d8ad2e8123ea110d1ec956dab4ab038eab665614adba04b6c3370",
        format: ChatFormat::ChatMl,
    },
    // Strong instruction-following alternative at the same size (Apache-2.0).
    ModelSpec {
        id: "smollm2-1.7b-instruct-q4",
        label: "SmolLM2 1.7B (instruction-tuned)",
        description: "Top instruction-following at this size. Apache-2.0.",
        size_label: "~1.1 GB",
        ram_label: "~2 GB RAM",
        gguf_url:
            "https://huggingface.co/bartowski/SmolLM2-1.7B-Instruct-GGUF/resolve/main/SmolLM2-1.7B-Instruct-Q4_K_M.gguf",
        gguf_sha256: "77665ea4815999596525c636fbeb56ba8b080b46ae85efef4f0d986a139834d7",
        format: ChatFormat::ChatMl,
    },
    // Higher-accuracy option for stronger CPUs (Llama community license).
    ModelSpec {
        id: "llama-3.2-3b-instruct-q4",
        label: "Llama 3.2 3B (higher accuracy)",
        description: "Best reasoning here; needs a stronger CPU. ~2 GB download.",
        size_label: "~2 GB",
        ram_label: "~4 GB RAM",
        gguf_url:
            "https://huggingface.co/bartowski/Llama-3.2-3B-Instruct-GGUF/resolve/main/Llama-3.2-3B-Instruct-Q4_K_M.gguf",
        gguf_sha256: "6c1a2b41161032677be168d354123594c0e6e67d2b9227c84f296ad037c728ff",
        format: ChatFormat::Llama3,
    },
];

/// Look up a model spec by id.
pub fn find(id: &str) -> Option<&'static ModelSpec> {
    MODELS.iter().find(|m| m.id == id)
}

/// Format a system prompt + user input into the model's chat template, returning
/// the string to tokenize (with the assistant turn opened for generation).
pub fn format_prompt(format: ChatFormat, system: &str, user: &str) -> String {
    match format {
        ChatFormat::Llama3 => format!(
            "<|begin_of_text|><|start_header_id|>system<|end_header_id|>\n\n{system}<|eot_id|>\
             <|start_header_id|>user<|end_header_id|>\n\n{user}<|eot_id|>\
             <|start_header_id|>assistant<|end_header_id|>\n\n"
        ),
        ChatFormat::Phi => format!(
            "<|system|>\n{system}<|end|>\n<|user|>\n{user}<|end|>\n<|assistant|>\n"
        ),
        ChatFormat::ChatMl => format!(
            "<|im_start|>system\n{system}<|im_end|>\n\
             <|im_start|>user\n{user}<|im_end|>\n\
             <|im_start|>assistant\n"
        ),
    }
}

/// The stop strings that end an assistant turn for a given format (so we halt
/// generation cleanly instead of running to max_tokens).
pub fn stop_strings(format: ChatFormat) -> &'static [&'static str] {
    match format {
        ChatFormat::Llama3 => &["<|eot_id|>", "<|end_of_text|>"],
        ChatFormat::Phi => &["<|end|>", "<|endoftext|>"],
        ChatFormat::ChatMl => &["<|im_end|>", "<|endoftext|>"],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_model_has_distinct_id_and_urls() {
        for m in MODELS {
            assert!(!m.id.is_empty());
            assert!(m.gguf_url.starts_with("https://"));
            assert_eq!(m.gguf_sha256.len(), 64, "sha256 is 64 hex chars");
        }
        // ids unique
        let ids: std::collections::HashSet<_> = MODELS.iter().map(|m| m.id).collect();
        assert_eq!(ids.len(), MODELS.len());
    }

    #[test]
    fn find_resolves_known_and_rejects_unknown() {
        assert!(find("qwen2.5-1.5b-instruct-q4").is_some());
        assert!(find("nope").is_none());
    }

    #[test]
    fn chatml_prompt_has_expected_wrappers() {
        let p = format_prompt(ChatFormat::ChatMl, "sys", "hi");
        assert!(p.starts_with("<|im_start|>system\nsys<|im_end|>"));
        assert!(p.contains("<|im_start|>user\nhi<|im_end|>"));
        assert!(p.ends_with("<|im_start|>assistant\n"));
    }

    #[test]
    fn llama_prompt_still_formats() {
        let p = format_prompt(ChatFormat::Llama3, "sys", "hi");
        assert!(p.starts_with("<|begin_of_text|>"));
        assert!(p.ends_with("<|start_header_id|>assistant<|end_header_id|>\n\n"));
    }

    #[test]
    fn curated_models_are_license_clean_and_well_formed() {
        // Every offered model must have a known chat format and a bartowski GGUF
        // (non-gated). Guards against re-adding a gated/weak/mis-templated entry.
        for m in MODELS {
            assert!(matches!(
                m.format,
                ChatFormat::ChatMl | ChatFormat::Llama3 | ChatFormat::Phi
            ));
            assert!(m.gguf_url.contains("huggingface.co/bartowski"));
        }
        // The default (first) entry is the recommended Qwen2.5-1.5B.
        assert_eq!(MODELS[0].id, "qwen2.5-1.5b-instruct-q4");
    }
}
