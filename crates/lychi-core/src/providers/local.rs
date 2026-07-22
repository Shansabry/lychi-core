//! Bundled local-AI provider — CPU inference via a statically-linked llama.cpp
//! (the `llama-cpp-2` crate). Feature-gated (`local-ai`), off by default.
//!
//! Standards notes:
//! - llama.cpp requires ONE process-global backend init; we hold it in a
//!   `OnceLock` (`backend()`).
//! - The multi-GB model is loaded ONCE and kept RESIDENT in a process-global
//!   cache keyed by model id (`ArcSwap`), so the config-save-triggered provider
//!   rebuild doesn't reload it.
//! - The forward pass is CPU-bound *blocking* work, so it runs on
//!   `spawn_blocking` — never on a tokio worker (same rule as the keyring).
//! - Threading is a first-class llama.cpp context parameter (`with_n_threads`),
//!   not a global rayon pool — so inference parallelism is tuned independently
//!   of the rest of the app.
//! - The GGUF's *embedded* tokenizer + architecture are used directly (llama.cpp
//!   reads both from the file), so there's no separate tokenizer download and no
//!   per-architecture loader branching — adding a model is one registry entry.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use arc_swap::ArcSwapOption;
use async_trait::async_trait;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;

use crate::error::LychiError;
use crate::intent::prompt;
use crate::providers::local_models::{self, ChatFormat, ModelSpec};

use super::{
    AiProvider, AiResponse, AiRoute, CancellationToken, ChatMessage, EventStream, Role, StopReason,
    StreamEvent, ToolDef,
};

/// The one-time, process-global llama.cpp backend. `LlamaBackend::init()` sets up
/// ggml/llama globals and must be called exactly once for the process lifetime.
/// We also suppress llama.cpp's verbose internal logging (graph/sched dumps) so
/// it doesn't drown our own tracing output.
fn backend() -> Result<&'static LlamaBackend, LychiError> {
    static BACKEND: OnceLock<Result<LlamaBackend, String>> = OnceLock::new();
    BACKEND
        .get_or_init(|| {
            llama_cpp_2::send_logs_to_tracing(
                llama_cpp_2::LogOptions::default().with_logs_enabled(false),
            );
            LlamaBackend::init().map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| LychiError::Ai(format!("llama backend init: {e}")))
}

/// A loaded, resident model: the llama.cpp model handle + its chat format. The
/// model is immutable and `Send + Sync`; a fresh context (which owns the mutable
/// KV cache) is created per generation, so nothing mutable is shared here.
struct LoadedModel {
    id: String,
    model: LlamaModel,
    format: ChatFormat,
}

/// Process-global resident-model cache. A provider rebuild (on config save)
/// reuses the loaded model when the id is unchanged, avoiding a multi-GB reload.
static RESIDENT: ArcSwapOption<LoadedModel> = ArcSwapOption::const_empty();

/// Serializes generation. llama.cpp inference for one model is single-flight in
/// our usage (routing/ask are not concurrent); the mutex makes that explicit and
/// keeps the resident model's use race-free across the async boundary.
static INFER_LOCK: Mutex<()> = Mutex::new(());

/// Load state of the resident model, so the UI can show a "loading" indicator
/// (a multi-GB model takes seconds to load into RAM). 0=idle, 1=loading,
/// 2=ready, 3=failed.
static LOAD_STATE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// The model's current load state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadState {
    Idle,
    Loading,
    Ready,
    Failed,
}

/// Read the current model load state (for the status-bar indicator).
pub fn load_state() -> LoadState {
    match LOAD_STATE.load(std::sync::atomic::Ordering::Acquire) {
        1 => LoadState::Loading,
        2 => LoadState::Ready,
        3 => LoadState::Failed,
        _ => LoadState::Idle,
    }
}

fn set_load_state(s: LoadState) {
    let v = match s {
        LoadState::Idle => 0,
        LoadState::Loading => 1,
        LoadState::Ready => 2,
        LoadState::Failed => 3,
    };
    LOAD_STATE.store(v, std::sync::atomic::Ordering::Release);
}

/// Preload a model into the resident cache (warmup at startup, so the first
/// query isn't slow). Blocking — call from a spawn_blocking / dedicated thread.
/// Updates `load_state()` throughout AND returns the terminal state, so the
/// caller can react to the result without reaching back into the global.
pub fn warmup(spec: &ModelSpec, gguf_path: &PathBuf) -> LoadState {
    // Already the resident model → nothing to do.
    if let Some(existing) = RESIDENT.load_full()
        && existing.id == spec.id
    {
        set_load_state(LoadState::Ready);
        return LoadState::Ready;
    }
    set_load_state(LoadState::Loading);
    match load_resident(spec, gguf_path) {
        Ok(_) => {
            set_load_state(LoadState::Ready);
            LoadState::Ready
        }
        Err(e) => {
            tracing::warn!("[local-ai] warmup failed: {e}");
            set_load_state(LoadState::Failed);
            LoadState::Failed
        }
    }
}

/// Load (or reuse the cached) model for `spec` from `path`. Called from a
/// blocking context (the sync factory / a spawn_blocking closure).
fn load_resident(spec: &ModelSpec, gguf_path: &PathBuf) -> Result<Arc<LoadedModel>, LychiError> {
    // Reuse if the resident model is already this one.
    if let Some(existing) = RESIDENT.load_full()
        && existing.id == spec.id
    {
        return Ok(existing);
    }

    // Actually loading now (lazy path or a model switch) — reflect it in the UI.
    set_load_state(LoadState::Loading);
    let backend = backend()?;

    // CPU-only: no GPU layers. llama.cpp reads the architecture + tokenizer from
    // the GGUF itself, so there's no per-model branching or side tokenizer file.
    let model_params = LlamaModelParams::default().with_n_gpu_layers(0);
    let model = LlamaModel::load_from_file(backend, gguf_path, &model_params)
        .map_err(|e| LychiError::Ai(format!("load model {}: {e}", gguf_path.display())))?;

    let loaded = Arc::new(LoadedModel {
        id: spec.id.to_string(),
        model,
        format: spec.format,
    });
    RESIDENT.store(Some(loaded.clone()));
    set_load_state(LoadState::Ready);
    tracing::info!("[local-ai] model '{}' loaded and resident", spec.id);
    Ok(loaded)
}

/// Upper bound on the context window (prompt + generation). Each call sizes its
/// context to what it actually needs, clamped to this ceiling; a small model
/// doing routing/short-ask never needs more.
const N_CTX_MAX: u32 = 8192;

/// Threads for the CPU forward pass. Small quantized models are memory-bandwidth
/// bound, so a moderate count (not `nproc`) is fastest; llama.cpp itself clamps.
fn n_threads() -> i32 {
    // Half the logical cores, clamped to [1, 8] — a good default for 0.5-1.5B on
    // desktop CPUs (past this, sync overhead outweighs the gain).
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    ((cores / 2).clamp(1, 8)) as i32
}

/// Run one blocking generation against a loaded model. Pure CPU work — MUST be
/// called inside `spawn_blocking`. `grammar` (GBNF) constrains the output when
/// `Some` — used by the routing path to force a valid JSON object so a small
/// model can't emit trailing prose or malformed JSON.
fn run_generation(
    model: &LoadedModel,
    system: &str,
    user: &str,
    max_tokens: u32,
    grammar: Option<&str>,
) -> Result<String, LychiError> {
    generate_inner(model, system, user, max_tokens, grammar, None).map(|(out, _)| out)
}

/// The generation core, returning `(text, decoded_token_count)` so the benchmark
/// can compute an exact tokens/sec. `run_generation` is the thin public wrapper.
///
/// `on_delta`, when `Some`, is called with each newly-decoded text fragment as it
/// generates (streaming). Returning `false` from it requests cancellation — the
/// loop stops and returns what it has. Grammar-constrained calls (routing / tool
/// calls) pass `None` (the JSON isn't meaningful to stream token-by-token).
fn generate_inner(
    model: &LoadedModel,
    system: &str,
    user: &str,
    max_tokens: u32,
    grammar: Option<&str>,
    mut on_delta: Option<&mut dyn FnMut(&str) -> bool>,
) -> Result<(String, usize), LychiError> {
    let _guard = INFER_LOCK
        .lock()
        .map_err(|_| LychiError::Ai("inference lock poisoned".into()))?;

    let backend = backend()?;
    let prompt_str = local_models::format_prompt(model.format, system, user);

    // Tokenize the prompt (BOS handled per the model's GGUF metadata).
    let tokens = model
        .model
        .str_to_token(&prompt_str, AddBos::Always)
        .map_err(|e| LychiError::Ai(format!("tokenize: {e}")))?;

    // The context (and batch) must be large enough to hold the whole prompt plus
    // headroom for generation. The routing system prompt can be sizable, so size
    // n_ctx/n_batch to the prompt (capped) rather than a fixed 512 — a prompt
    // exceeding n_batch triggers a hard GGML assert (process abort), not a
    // recoverable error, so this must be right.
    let need = tokens.len() as u32 + max_tokens + 16;
    let ctx_size = need.clamp(512, N_CTX_MAX);
    if tokens.len() as u32 >= N_CTX_MAX {
        return Err(LychiError::Ai(format!(
            "prompt too long: {} tokens (max {N_CTX_MAX})",
            tokens.len()
        )));
    }

    // Fresh context per call → a clean KV cache, no cross-request bleed. n_batch
    // == n_ctx so the entire prompt prefills in one submission.
    let threads = n_threads();
    let ctx_params = LlamaContextParams::default()
        .with_n_ctx(std::num::NonZeroU32::new(ctx_size))
        .with_n_batch(ctx_size)
        .with_n_threads(threads)
        .with_n_threads_batch(threads);
    let mut ctx = model
        .model
        .new_context(backend, ctx_params)
        .map_err(|e| LychiError::Ai(format!("create context: {e}")))?;

    // Prefill: submit the whole prompt in one batch, mark the last token for
    // logits (that's where the first sampled token comes from).
    let mut batch = LlamaBatch::new(tokens.len().max(1), 1);
    let last_idx = tokens.len() as i32 - 1;
    for (i, tok) in tokens.iter().enumerate() {
        batch
            .add(*tok, i as i32, &[0], i as i32 == last_idx)
            .map_err(|e| LychiError::Ai(format!("batch add: {e}")))?;
    }
    ctx.decode(&mut batch)
        .map_err(|e| LychiError::Ai(format!("prefill decode: {e}")))?;

    // Sampler chain. When a grammar is supplied (routing → JSON), use the
    // known-good minimal recipe: grammar masks invalid tokens, then greedy picks
    // the highest-probability allowed token. Deterministic — which is what we
    // want for routing anyway — and avoids the empty-stack grammar assert that
    // temp/top-p/dist can trigger by pruning the grammar's allowed set. For free
    // text (ask), use temp+top-p sampling with a light repetition penalty.
    let mut sampler = if let Some(g) = grammar {
        LlamaSampler::chain_simple([
            LlamaSampler::grammar(&model.model, g, "root")
                .map_err(|e| LychiError::Ai(format!("grammar: {e}")))?,
            LlamaSampler::greedy(),
        ])
    } else {
        LlamaSampler::chain_simple([
            LlamaSampler::penalties(64, 1.1, 0.0, 0.0),
            LlamaSampler::temp(0.2),
            LlamaSampler::top_p(0.9, 1),
            LlamaSampler::dist(42),
        ])
    };

    let stops = local_models::stop_strings(model.format);
    // Accumulate raw token bytes and decode to UTF-8 at the boundaries — a token
    // can split a multi-byte codepoint, so decoding the running byte buffer
    // (lossily) each step is both correct and cheap for short outputs.
    let mut bytes: Vec<u8> = Vec::new();
    let mut n_cur = batch.n_tokens();
    let mut decoded = 0usize;
    // How many chars of the decoded text we've already streamed to `on_delta`,
    // so each step emits only the newly-produced suffix.
    let mut emitted_chars = 0usize;

    for _ in 0..max_tokens {
        // Sample from the logits of the last decoded position. `sample()` already
        // calls the sampler's `accept` internally (it wraps llama_sampler_sample),
        // so we must NOT accept again — a double-accept advances the grammar
        // sampler's stack twice and trips GGML_ASSERT(!stacks.empty()).
        let token = sampler.sample(&ctx, batch.n_tokens() - 1);

        if model.model.is_eog_token(token) {
            break;
        }
        decoded += 1;

        // Detokenize this token to raw bytes and append (special tokens rendered
        // as plaintext so stop-strings like `<|im_end|>` are matchable).
        if let Ok(piece) = model.model.token_to_piece_bytes(token, 256, true, None) {
            bytes.extend_from_slice(&piece);
        }

        // Stop-string guard (belt-and-suspenders alongside the EOG check).
        let text = String::from_utf8_lossy(&bytes);
        if let Some(idx) = stops.iter().filter_map(|s| text.find(s)).min() {
            // Flush any not-yet-emitted text before the stop marker.
            if let Some(cb) = on_delta.as_deref_mut() {
                let final_text = &text[..idx];
                if let Some(tail) = suffix_from_char(final_text, emitted_chars) {
                    cb(tail);
                }
            }
            return Ok((text[..idx].trim().to_string(), decoded));
        }

        // Stream the newly-decoded suffix (only complete chars — a token can
        // split a codepoint, so `from_utf8_lossy` may end in a replacement char
        // that a later token completes; emitting on char boundaries avoids
        // showing the � placeholder).
        if let Some(cb) = on_delta.as_deref_mut()
            && let Some(delta) = suffix_from_char(&text, emitted_chars)
            && !delta.is_empty()
        {
            emitted_chars = text.chars().count();
            if !cb(delta) {
                // Cancellation requested — return what we have.
                return Ok((text.trim().to_string(), decoded));
            }
        }

        // Feed the sampled token back in for the next step.
        batch.clear();
        batch
            .add(token, n_cur, &[0], true)
            .map_err(|e| LychiError::Ai(format!("batch add: {e}")))?;
        n_cur += 1;
        ctx.decode(&mut batch)
            .map_err(|e| LychiError::Ai(format!("decode: {e}")))?;
    }

    // Flush any final not-yet-emitted text.
    if let Some(cb) = on_delta.as_deref_mut() {
        let text = String::from_utf8_lossy(&bytes);
        if let Some(tail) = suffix_from_char(&text, emitted_chars) {
            cb(tail);
        }
    }

    Ok((String::from_utf8_lossy(&bytes).trim().to_string(), decoded))
}

/// Return the substring of `s` starting at char index `from`, or `None` if `from`
/// is at/after the end. Used to stream only the newly-generated suffix.
fn suffix_from_char(s: &str, from: usize) -> Option<&str> {
    let byte_idx = s.char_indices().nth(from).map(|(i, _)| i);
    match byte_idx {
        Some(i) => Some(&s[i..]),
        None => None,
    }
}

/// The bundled local-AI provider. Holds the resolved model spec + gguf path;
/// weights load lazily (or reuse the resident cache) on first inference.
pub struct LocalClient {
    spec: &'static ModelSpec,
    gguf_path: PathBuf,
    max_tokens: u32,
}

impl LocalClient {
    /// Construct a client for the model at `path`. Does NOT load weights (that
    /// happens lazily on first inference, so app start / provider rebuild stays
    /// fast); `path` is the resolved GGUF file.
    pub fn load(path: PathBuf, max_tokens: u32) -> Result<Self, LychiError> {
        let id = path.file_stem().and_then(|n| n.to_str()).unwrap_or_default();
        let spec = local_models::find(id).ok_or_else(|| {
            LychiError::Ai(format!("unknown local model id '{id}' (not in registry)"))
        })?;
        Ok(Self {
            spec,
            gguf_path: path,
            max_tokens,
        })
    }

    /// Generate text under an optional output grammar.
    async fn generate(
        &self,
        system_prompt: &str,
        user_input: &str,
        grammar_mode: GrammarMode,
    ) -> Result<String, LychiError> {
        let spec = self.spec;
        let path = self.gguf_path.clone();
        let (system, user) = (system_prompt.to_string(), user_input.to_string());
        let max = self.max_tokens;

        // Load (or reuse resident) + run the forward pass off the async runtime.
        tokio::task::spawn_blocking(move || {
            let model = load_resident(spec, &path)?;
            let grammar = grammar_mode.grammar()?;
            run_generation(&model, &system, &user, max, grammar.as_deref())
        })
        .await
        .map_err(|e| LychiError::Ai(format!("inference task panicked: {e}")))?
    }

}

/// How to constrain the model's output for a given call.
enum GrammarMode {
    /// Free text — no grammar (ask/answer path).
    Free,
    /// Routing: the output must be `{"action_id": <one of these>, "args": "..."}`.
    /// Constraining `action_id` to the actual known actions structurally prevents
    /// the model from inventing a nonexistent action — the biggest quality win for
    /// small models, and correct for any model.
    Route(Vec<String>),
    /// Tool-calling (the `chat` primitive): the output is EITHER a tool call
    /// `{"tool": <one of these>, "args": "..."}` OR a final answer
    /// `{"answer": "..."}`. The enum-constrained `tool` name means the small
    /// local model can only pick a real tool; the two-way union lets it also just
    /// answer. One tool call per turn — the coordinator loops.
    ///
    /// TODO(local-tools): the local `chat` impl doesn't yet drive this grammar —
    /// local AI currently answers (tools=[]) but can't call tools. Wiring this is
    /// the remaining piece of local-model agent support; `tool_grammar` + this
    /// variant are the scaffolding, kept intentionally.
    #[allow(dead_code)]
    Tool(Vec<String>),
}

impl GrammarMode {
    /// Build the GBNF grammar string for this mode (None = free text). The
    /// route/tool grammars are cached per name-set so we don't rebuild each call.
    fn grammar(&self) -> Result<Option<String>, LychiError> {
        match self {
            GrammarMode::Free => Ok(None),
            GrammarMode::Route(actions) => Ok(Some(route_grammar(actions)?)),
            GrammarMode::Tool(tools) => Ok(Some(tool_grammar(tools)?)),
        }
    }
}

/// Build a GBNF grammar for a tool-calling response: EITHER
/// `{"tool": (<enum of tool names>), "args": <string>}` (call a tool) OR
/// `{"answer": <string>}` (final answer). `tool` is enum-constrained to real
/// tool names. Cached per tool-set. Empty tool set → answer-only grammar.
fn tool_grammar(tools: &[String]) -> Result<String, LychiError> {
    static CACHE: OnceLock<Mutex<std::collections::HashMap<String, String>>> = OnceLock::new();
    let key = tools.join("\u{1}");
    let cache = CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    if let Some(g) = cache.lock().ok().and_then(|c| c.get(&key).cloned()) {
        return Ok(g);
    }

    let answer_schema = r#"{"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"],"additionalProperties":false}"#;
    let schema = if tools.is_empty() {
        answer_schema.to_string()
    } else {
        let enum_json = serde_json::to_string(tools)
            .map_err(|e| LychiError::Ai(format!("build tool schema: {e}")))?;
        let call_schema = format!(
            r#"{{"type":"object","properties":{{"tool":{{"type":"string","enum":{enum_json}}},"args":{{"type":"string"}}}},"required":["tool","args"],"additionalProperties":false}}"#
        );
        format!(r#"{{"anyOf":[{call_schema},{answer_schema}]}}"#)
    };
    let grammar = llama_cpp_2::json_schema_to_grammar(&schema)
        .map_err(|e| LychiError::Ai(format!("build tool grammar: {e}")))?;
    if let Ok(mut c) = cache.lock() {
        c.insert(key, grammar.clone());
    }
    Ok(grammar)
}

/// Build a GBNF grammar for a routing response constrained to the given action
/// IDs: `{"action_id": ("a"|"b"|...), "args": <string>}`. We build the JSON
/// schema (an object with an `enum`-constrained `action_id`) and hand it to
/// llama.cpp's official `json_schema_to_grammar`, so the grammar is always valid.
fn route_grammar(actions: &[String]) -> Result<String, LychiError> {
    // Cache by the action-set (it's stable across a session but can change if the
    // registry changes). Keyed by a join of the ids.
    static CACHE: OnceLock<Mutex<std::collections::HashMap<String, String>>> = OnceLock::new();
    let key = actions.join("\u{1}");
    let cache = CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    if let Some(g) = cache.lock().ok().and_then(|c| c.get(&key).cloned()) {
        return Ok(g);
    }

    // action_id is an enum of the known IDs; args is any string. additional
    // properties disallowed so no stray fields.
    let enum_json = serde_json::to_string(actions)
        .map_err(|e| LychiError::Ai(format!("build route schema: {e}")))?;
    let schema = format!(
        r#"{{"type":"object","properties":{{"action_id":{{"type":"string","enum":{enum_json}}},"args":{{"type":"string"}}}},"required":["action_id","args"],"additionalProperties":false}}"#
    );
    let grammar = llama_cpp_2::json_schema_to_grammar(&schema)
        .map_err(|e| LychiError::Ai(format!("build route grammar: {e}")))?;
    if let Ok(mut c) = cache.lock() {
        c.insert(key, grammar.clone());
    }
    Ok(grammar)
}

#[async_trait]
impl AiProvider for LocalClient {
    async fn route_intent(
        &self,
        input: &str,
        known_actions: &[&str],
    ) -> Result<AiRoute, LychiError> {
        match self.route_or_plan(input, known_actions, None).await? {
            AiResponse::SingleRoute(route) => Ok(route),
            AiResponse::Plan(_) => Err(LychiError::Ai(
                "AI returned a plan but single route was expected".to_string(),
            )),
        }
    }

    async fn route_or_plan(
        &self,
        input: &str,
        known_actions: &[&str],
        context_hint: Option<&str>,
    ) -> Result<AiResponse, LychiError> {
        let sys_prompt = prompt::system_prompt(known_actions, context_hint);
        // Routing → constrain the output to `{"action_id": <known>, "args": ...}`
        // so the model can only pick a real action (and always emits valid JSON).
        let actions = known_actions.iter().map(|s| s.to_string()).collect();
        let response = self
            .generate(&sys_prompt, input, GrammarMode::Route(actions))
            .await?;
        tracing::debug!(
            provider = "local",
            model = %self.spec.id,
            "[ai] raw response: {response}"
        );
        prompt::parse_ai_response(&response, known_actions, input)
    }

    async fn health_check(&self) -> bool {
        self.gguf_path.exists()
    }

    fn name(&self) -> &str {
        "local"
    }

    async fn answer_question(
        &self,
        system_prompt: &str,
        question: &str,
    ) -> Result<String, LychiError> {
        // Free-form answer → no grammar constraint.
        self.generate(system_prompt, question, GrammarMode::Free).await
    }

    /// Streaming, grammar-constrained tool-calling chat as a normalized event
    /// stream. The bundled small model is single-turn oriented, so the
    /// conversation is flattened into a system + user prompt. With no tools it
    /// streams free-text `TextDelta`s; with tools it runs the tool-grammar (one
    /// call OR an answer per turn — the coordinator loops).
    ///
    /// The sync llama.cpp loop runs on `spawn_blocking` and pushes events over a
    /// bounded channel drained by a `ReceiverStream` (the standard sync→async
    /// bridge). Cancellation: `spawn_blocking` can't be aborted by drop, so the
    /// loop polls `cancel` each token; a dropped consumer is detected because
    /// `blocking_send` returns `Err` once the receiver is gone.
    fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDef],
        cancel: CancellationToken,
    ) -> EventStream {
        use futures_util::StreamExt as _;
        use tokio_stream::wrappers::ReceiverStream;

        let (system, user) = flatten_conversation(messages);
        let spec = self.spec;
        let path = self.gguf_path.clone();
        let max = self.max_tokens;
        let model_id = self.spec.id.to_string();
        let tool_names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
        let has_tools = !tool_names.is_empty();

        // Bounded channel → backpressure (the generation thread blocks on a slow
        // consumer instead of buffering unbounded tokens).
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<StreamEvent, LychiError>>(16);

        tokio::task::spawn_blocking(move || {
            // Load (or reuse resident) weights.
            let model = match load_resident(spec, &path) {
                Ok(m) => m,
                Err(e) => {
                    let _ = tx.blocking_send(Err(e));
                    return;
                }
            };
            let _ = tx.blocking_send(Ok(StreamEvent::MessageStart { model: model_id.clone() }));

            if has_tools {
                // Tool mode: grammar-constrain to a tool call OR an answer (one
                // shot — grammar JSON isn't meaningful to stream token-by-token).
                let grammar = match tool_grammar(&tool_names) {
                    Ok(g) => Some(g),
                    Err(e) => { let _ = tx.blocking_send(Err(e)); return; }
                };
                let raw = match generate_inner(&model, &system, &user, max, grammar.as_deref(), None) {
                    Ok((out, _)) => out,
                    Err(e) => { let _ = tx.blocking_send(Err(e)); return; }
                };
                tracing::debug!(provider = "local", model = %model_id, "[ai] tool response: {raw}");
                match serde_json::from_str::<serde_json::Value>(&raw) {
                    Ok(v) if v["tool"].is_string() => {
                        let id = format!("local-{model_id}");
                        let name = v["tool"].as_str().unwrap_or_default().to_string();
                        let args = v["args"].as_str().unwrap_or_default().to_string();
                        let _ = tx.blocking_send(Ok(StreamEvent::ToolCallStart { id: id.clone(), name: name.clone() }));
                        let _ = tx.blocking_send(Ok(StreamEvent::ToolCallComplete { id, name, args }));
                        let _ = tx.blocking_send(Ok(StreamEvent::Done { stop_reason: StopReason::ToolUse, usage: None }));
                    }
                    Ok(v) => {
                        let answer = v["answer"].as_str().unwrap_or(&raw).to_string();
                        let _ = tx.blocking_send(Ok(StreamEvent::TextDelta(answer)));
                        let _ = tx.blocking_send(Ok(StreamEvent::Done { stop_reason: StopReason::EndTurn, usage: None }));
                    }
                    Err(e) => {
                        let _ = tx.blocking_send(Err(LychiError::Ai(format!("local tool JSON parse: {e} (raw: {raw})"))));
                    }
                }
                return;
            }

            // Pure chat: stream free text. `on_delta` pushes each fragment; it
            // returns false to stop when the consumer is gone OR cancel fired.
            let mut cb = |delta: &str| {
                if cancel.is_cancelled() {
                    return false;
                }
                // blocking_send Err ⇒ ReceiverStream dropped ⇒ stop generating.
                tx.blocking_send(Ok(StreamEvent::TextDelta(delta.to_string()))).is_ok()
            };
            match generate_inner(&model, &system, &user, max, None, Some(&mut cb)) {
                Ok(_) => {
                    let _ = tx.blocking_send(Ok(StreamEvent::Done { stop_reason: StopReason::EndTurn, usage: None }));
                }
                Err(e) => {
                    let _ = tx.blocking_send(Err(e));
                }
            }
        });

        ReceiverStream::new(rx).boxed()
    }
}

/// Flatten a chat history into a (system, user) pair for the single-turn local
/// model. System messages are joined into the system prompt; the rest of the
/// conversation (user/assistant/tool turns) is rendered into the user slot as a
/// transcript so the model still sees context.
fn flatten_conversation(messages: &[ChatMessage]) -> (String, String) {
    let mut system = Vec::new();
    let mut transcript = Vec::new();
    for m in messages {
        match m.role {
            Role::System => system.push(m.content.clone()),
            Role::User => transcript.push(format!("User: {}", m.content)),
            Role::Assistant => {
                if !m.content.is_empty() {
                    transcript.push(format!("Assistant: {}", m.content));
                }
                for tc in &m.tool_calls {
                    transcript.push(format!("Assistant called tool `{}` with: {}", tc.name, tc.args));
                }
            }
            Role::Tool => {
                let tag = if m.is_error { "Tool error" } else { "Tool result" };
                transcript.push(format!("{tag}: {}", m.content));
            }
        }
    }
    (system.join("\n\n"), transcript.join("\n"))
}


#[cfg(test)]
mod bench {
    use super::*;
    use crate::providers::local_models::MODELS;
    use std::time::Instant;

    /// Benchmark every DOWNLOADED model — reports load time + decode tok/s.
    /// Ignored by default (needs downloaded weights + is slow); run with:
    ///   cargo test -p lychi-core --features local-ai --release bench_local_models -- --ignored --nocapture
    #[test]
    #[ignore]
    fn bench_local_models() {
        let dir = crate::paths::models_dir();
        let mut ran = 0;
        println!("\n=== Local AI benchmark (llama.cpp, CPU) ===");
        println!("threads: {}\n", n_threads());
        for spec in MODELS {
            let path = dir.join(spec.gguf_filename());
            if !path.exists() {
                println!("· {:<28} — not downloaded, skipping", spec.id);
                continue;
            }
            print!("· {:<28} loading… ", spec.id);
            let t = Instant::now();
            let model = match load_resident(spec, &path) {
                Ok(m) => m,
                Err(e) => {
                    println!("LOAD FAILED: {e}");
                    continue;
                }
            };
            let load_s = t.elapsed().as_secs_f64();

            // A prompt that generates a long answer so we measure sustained decode
            // (not fixed overhead), timing only the decode loop and counting the
            // exact tokens produced → a true tokens/sec.
            let t1 = Instant::now();
            let (out, ntok) = generate_inner(
                &model,
                "You are a helpful assistant.",
                "Write a detailed paragraph about the history of the Eiffel Tower.",
                128,
                None,
                None,
            )
            .unwrap_or_else(|e| (format!("<gen failed: {e}>"), 0));
            let gen_s = t1.elapsed().as_secs_f64();
            let tps = ntok as f64 / gen_s.max(1e-9);

            println!("load {load_s:.2}s | {tps:.1} tok/s ({ntok} tokens in {gen_s:.2}s)");
            println!("    → {:?}", out.chars().take(80).collect::<String>());
            ran += 1;
        }
        println!();
        assert!(ran > 0, "no models downloaded — download at least one first");
    }

    /// Regression for the GGML_ASSERT(n_tokens_all <= n_batch) process-abort:
    /// a prompt well over the old fixed 512 n_batch must generate, not crash.
    /// (This is the real-world routing/ask case that the short benchmark missed.)
    #[test]
    #[ignore]
    fn large_prompt_does_not_abort() {
        let dir = crate::paths::models_dir();
        let Some(spec) = MODELS.iter().find(|s| dir.join(s.gguf_filename()).exists()) else {
            eprintln!("no model downloaded — skipping");
            return;
        };
        let model = load_resident(spec, &dir.join(spec.gguf_filename())).unwrap();
        // ~1200-token system prompt (well past 512), like the real router prompt,
        // WITH the action-constrained route grammar — exactly the path that
        // failed live (grammar assert + oversized-prompt abort).
        let big = format!(
            "You are an intent router. Respond with a JSON object. {}",
            "Consider the available actions carefully. ".repeat(180)
        );
        let actions: Vec<String> = ["open", "web", "ask", "run", "calc"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let grammar = route_grammar(&actions).expect("grammar builds");
        let (out, _) = generate_inner(&model, &big, "open firefox", 48, Some(&grammar), None)
            .expect("large prompt must not abort");
        println!("route-grammar output: {out:?}");
        // The grammar guarantees a parseable object with a KNOWN action_id.
        let v: serde_json::Value = serde_json::from_str(out.trim())
            .expect("grammar output must be valid JSON");
        assert!(v.is_object(), "output must be a JSON object");
        let action = v["action_id"].as_str().unwrap_or("");
        assert!(
            actions.iter().any(|a| a == action),
            "action_id {action:?} must be one of the constrained actions"
        );
    }

    #[test]
    fn route_grammar_builds_and_constrains() {
        // Pure (no model needed): the schema→grammar conversion must succeed and
        // the grammar must mention the action literals.
        let actions: Vec<String> = ["open", "ask", "web"].iter().map(|s| s.to_string()).collect();
        let g = route_grammar(&actions).expect("route grammar builds");
        assert!(g.contains("open") && g.contains("ask") && g.contains("web"));
    }

    #[test]
    fn tool_grammar_constrains_to_tool_or_answer() {
        // Pure (no model): the tool grammar must mention the tool names and allow
        // an answer branch. Empty tool set → answer-only.
        let tools: Vec<String> = ["weather", "open"].iter().map(|s| s.to_string()).collect();
        let g = tool_grammar(&tools).expect("tool grammar builds");
        assert!(g.contains("weather") && g.contains("open"), "tool names constrained");
        assert!(g.contains("answer"), "answer branch present");

        let empty: Vec<String> = vec![];
        let g0 = tool_grammar(&empty).expect("answer-only grammar builds");
        assert!(g0.contains("answer"));
    }

    #[test]
    fn flatten_conversation_renders_history_and_system() {
        use crate::providers::{ChatMessage, Role, ToolCall};
        let msgs = vec![
            ChatMessage::system("You are helpful."),
            ChatMessage::user("open firefox"),
            ChatMessage {
                role: Role::Assistant,
                content: "Opening it".into(),
                tool_call_id: None,
                tool_calls: vec![ToolCall { id: "t1".into(), name: "open".into(), args: "firefox".into() }],
                is_error: false,
            },
            ChatMessage::tool_result("t1", "opened", false),
        ];
        let (system, transcript) = flatten_conversation(&msgs);
        assert_eq!(system, "You are helpful.");
        assert!(transcript.contains("User: open firefox"));
        assert!(transcript.contains("called tool `open`"));
        assert!(transcript.contains("Tool result: opened"));
    }
}
