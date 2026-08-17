//! LIVE agent evals — Raycast-style "which tool does the model actually call"
//! assertions against the REAL model catalog and a REAL provider, with a
//! recording executor so nothing ever executes on the machine.
//!
//! The deterministic layers have their own batteries (selection recall and
//! dispatch round-trips in `state.rs`, grammar↔parser drift tests per handler,
//! wire round-trips). What those cannot catch is the MODEL's tool choice — the
//! preset-shadowing bug class, trivia burning a tool round-trip, a freshness
//! question answered from stale memory. This suite catches exactly that, and
//! it is the harness that would have flagged today's regressions mechanically.
//!
//! COSTS REAL TOKENS + NETWORK, so it is `#[ignore]` and skipped without a key:
//!
//! ```sh
//! GROQ_API_KEY=gsk_… cargo test -p lychi-app evals -- --ignored --nocapture
//! LYCHI_EVAL_MODEL=meta-llama/llama-4-scout-17b-16e-instruct \
//!     GROQ_API_KEY=… cargo test -p lychi-app evals -- --ignored --nocapture
//! ```
//!
//! Model nondeterminism is real: a failing case is a signal to look, not
//! automatically a regression. Keep cases unambiguous.

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use lychi_core::coordinator::{
        Coordinator, MaxSteps, Outcome, ResumeToken, Session, ToolExecutor, ToolOutcome,
        ToolOutputChannel,
    };
    use lychi_core::error::LychiError;
    use lychi_core::providers::{CancellationToken, ToolDef};

    /// The agent system prompt is OWNED by the frontend; extract it from the
    /// source at compile time so the evals can never drift from what ships.
    fn agent_system() -> String {
        const SRC: &str = include_str!("../../src/lib/stores/chat.svelte.ts");
        let start = SRC
            .find("AGENT_SYSTEM =")
            .and_then(|i| SRC[i..].find('"').map(|q| i + q + 1))
            .expect("AGENT_SYSTEM literal present");
        let end = start
            + SRC[start..]
                .find("\";")
                .expect("AGENT_SYSTEM literal terminated");
        SRC[start..end].replace("\\'", "'")
    }

    /// Records every call; returns canned, side-effect-free results shaped
    /// enough for the model to wrap up. NOTHING executes.
    struct RecordingExecutor {
        calls: Mutex<Vec<(String, String)>>,
    }
    #[async_trait]
    impl ToolExecutor for RecordingExecutor {
        async fn execute(
            &self,
            name: &str,
            args: &str,
            _output: Option<ToolOutputChannel>,
        ) -> Result<ToolOutcome, LychiError> {
            self.calls
                .lock()
                .unwrap()
                .push((name.to_string(), args.to_string()));
            let output = match name {
                "web_tools" if args.contains("search") => {
                    "Results:\n1. Example result — https://example.org — a relevant snippet \
                     answering the query."
                        .to_string()
                }
                "web_tools" => "Web content (untrusted): example page text.".to_string(),
                _ => "Done.".to_string(),
            };
            Ok(ToolOutcome::Ran {
                output,
                is_error: false,
                artifact: None,
                image: None,
            })
        }
        async fn run_approved(&self, _resume: ResumeToken) -> Result<String, LychiError> {
            Ok("Done.".to_string())
        }
    }

    /// What a case expects of the FIRST tool call the model makes.
    enum Expect {
        /// The model must answer with NO tool call at all.
        NoTool,
        /// First call hits this tool; when `action` is set, the JSON args'
        /// `action` must start with it.
        Tool {
            name: &'static str,
            action: Option<&'static str>,
        },
    }

    fn production_tools() -> Vec<ToolDef> {
        let path = std::env::temp_dir().join(format!("lychi-evals-{}.redb", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let db = lychi_core::db::open_database(&path).expect("temp db");
        let config = lychi_core::config::Config::default();
        let timer_state = lychi_core::action_registry::handlers::timer::new_timer_state();
        #[cfg(feature = "mpris")]
        let mpris = std::sync::Arc::new(tokio::sync::RwLock::new(None));
        let registry = crate::state::AppState::build_builtin_registry(
            &db,
            &config,
            &timer_state,
            #[cfg(feature = "mpris")]
            &mpris,
        );
        let tools = registry
            .model_catalog()
            .into_iter()
            .map(|m| ToolDef {
                name: m.name,
                description: m.description,
                mutates: m.mutates,
                mutating_actions: m.mutating_actions,
                input_schema: m.input_schema,
            })
            .collect();
        let _ = std::fs::remove_file(&path);
        tools
    }

    #[tokio::test]
    #[ignore = "live model eval — needs GROQ_API_KEY, costs tokens; run with --ignored"]
    async fn live_tool_choice_evals() {
        let Ok(key) = std::env::var("GROQ_API_KEY") else {
            eprintln!("SKIP: GROQ_API_KEY not set");
            return;
        };
        let model =
            std::env::var("LYCHI_EVAL_MODEL").unwrap_or_else(|_| "openai/gpt-oss-120b".to_string());
        let provider: Arc<dyn lychi_core::providers::AiProvider> =
            Arc::new(lychi_core::providers::byo::BYOClient::new(
                "groq",
                lychi_core::providers::byo::WireFormat::OpenAi,
                "https://api.groq.com/openai/v1/chat/completions",
                model.clone(),
                key,
                1024,
            ));
        let tools = production_tools();
        let system = agent_system();

        let cases: &[(&str, Expect)] = &[
            // Knowledge stays knowledge — no tool round-trip on trivia/math.
            ("what is a dolphin?", Expect::NoTool),
            ("what is 17 * 23?", Expect::NoTool),
            // Freshness rule: changeable facts must search, never answer from
            // memory (the chief-minister bug class).
            (
                "who is the current prime minister of the uk?",
                Expect::Tool {
                    name: "web_tools",
                    action: Some("search"),
                },
            ),
            // Action requests hit their group + verb.
            (
                "add a note: buy milk tomorrow",
                Expect::Tool {
                    name: "personal_data",
                    action: Some("note_add"),
                },
            ),
            (
                "take a screenshot of my screen",
                Expect::Tool {
                    name: "system_control",
                    action: Some("screenshot"),
                },
            ),
            (
                "search the web for the tauri v2 window api",
                Expect::Tool {
                    name: "web_tools",
                    action: Some("search"),
                },
            ),
            (
                "pause the music",
                Expect::Tool {
                    name: "media_control",
                    action: None,
                },
            ),
            (
                "set the volume to 50 percent",
                Expect::Tool {
                    name: "system_control",
                    action: Some("system_volume"),
                },
            ),
            (
                "list my todos",
                Expect::Tool {
                    name: "personal_data",
                    action: Some("todo"),
                },
            ),
        ];

        let mut failures: Vec<String> = Vec::new();
        for (prompt, expect) in cases {
            let exec = Arc::new(RecordingExecutor {
                calls: Mutex::new(Vec::new()),
            });
            let coord = Coordinator::new(provider.clone(), exec.clone(), tools.clone())
                .with_stop(Arc::new(MaxSteps(3)));
            let (stream, handle) =
                coord.run(Session::new(&system, *prompt), CancellationToken::new());
            // Drain the event stream (required for the loop to progress).
            let mut stream = stream;
            use futures_util::StreamExt as _;
            while stream.next().await.is_some() {}
            let outcome = handle.wait().await;

            let calls = exec.calls.lock().unwrap().clone();
            let verdict = match (expect, calls.first()) {
                (Expect::NoTool, None) => Ok(()),
                (Expect::NoTool, Some((name, args))) => {
                    Err(format!("expected NO tool, called `{name}` {args}"))
                }
                (Expect::Tool { name, .. }, None) => {
                    Err(format!("expected `{name}`, called nothing"))
                }
                (Expect::Tool { name, action }, Some((got, args))) => {
                    if got != name {
                        Err(format!("expected `{name}`, called `{got}` {args}"))
                    } else if let Some(prefix) = action {
                        let got_action = serde_json::from_str::<serde_json::Value>(args)
                            .ok()
                            .and_then(|v| v["action"].as_str().map(String::from))
                            .unwrap_or_default();
                        if got_action.starts_with(prefix) {
                            Ok(())
                        } else {
                            Err(format!(
                                "right tool, wrong action: expected `{prefix}*`, got `{got_action}`"
                            ))
                        }
                    } else {
                        Ok(())
                    }
                }
            };
            match verdict {
                Ok(()) => eprintln!("PASS  {prompt:?}"),
                Err(why) => {
                    eprintln!("FAIL  {prompt:?} — {why}");
                    failures.push(format!("{prompt:?}: {why}"));
                }
            }
            if matches!(outcome, Outcome::Error { .. }) {
                eprintln!("  (note: run ended in a provider error)");
            }
        }

        eprintln!(
            "\n[evals] {}/{} passed on {model}",
            cases.len() - failures.len(),
            cases.len()
        );
        assert!(
            failures.is_empty(),
            "eval failures:\n{}",
            failures.join("\n")
        );
    }
}
