// ta-agent-ollama — Local model agent for Trusted Autonomy (v0.13.16).
//
// Implements a tool-use loop against any OpenAI-compatible endpoint
// (Ollama, llama.cpp server, vLLM, LM Studio, OpenAI API).
//
// ## Usage
//
// ```bash
// ta-agent-ollama --model qwen2.5-coder:7b [--base-url http://localhost:11434]
//                 [--context-file /path/to/context.md]
//                 [--memory-path /path/to/snapshot.md]
//                 [--memory-out /path/to/out.json]
// ```
//
// ## Protocol
//
// 1. Load goal context from --context-file or $TA_GOAL_CONTEXT
// 2. Emit "[goal started]" to stderr (TA watches for this sentinel)
// 3. Probe /v1/models — verify model exists; emit error if not
// 4. Test function-calling support with a lightweight probe call
// 5. If function calling: run full tool-use loop until model stops calling tools
// 6. If no function calling: fall back to CoT-with-parsing mode (best-effort)
// 7. On exit: flush memory writes to --memory-out / $TA_MEMORY_OUT
//
// ## Memory bridge
//
// Read  — $TA_MEMORY_PATH (snapshot written by TA before launch, env/context mode)
// Write — $TA_MEMORY_OUT  (ingested by TA after agent exits, exit-file mode)

mod client;
mod memory;
mod tools;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;

use client::OllamaClient;
use memory::MemoryBridge;
use tools::ToolSet;

#[derive(Parser, Debug)]
#[command(
    name = "ta-agent-ollama",
    about = "Trusted Autonomy local-model agent (OpenAI-compat tool-use loop)",
    long_about = "Runs a tool-use loop against any OpenAI-compatible endpoint.\n\
                  Reads goal context from --context-file or $TA_GOAL_CONTEXT.\n\
                  Emits '[goal started]' on stderr so TA can detect agent startup."
)]
struct Args {
    /// Model identifier (e.g., qwen2.5-coder:7b, phi4-mini, gpt-4o).
    #[arg(long, env = "TA_AGENT_MODEL")]
    model: String,

    /// OpenAI-compatible API base URL.
    #[arg(
        long,
        default_value = "http://localhost:11434",
        env = "TA_AGENT_BASE_URL"
    )]
    base_url: String,

    /// Path to a markdown context file to include in the system prompt.
    /// Falls back to $TA_GOAL_CONTEXT if not provided.
    #[arg(long, env = "TA_GOAL_CONTEXT")]
    context_file: Option<PathBuf>,

    /// Path to a memory snapshot file (markdown) written by TA before launch.
    /// Falls back to $TA_MEMORY_PATH.
    #[arg(long, env = "TA_MEMORY_PATH")]
    memory_path: Option<PathBuf>,

    /// Path where the agent should write new memory entries on exit (JSON array).
    /// Falls back to $TA_MEMORY_OUT.
    #[arg(long, env = "TA_MEMORY_OUT")]
    memory_out: Option<PathBuf>,

    /// Working directory for file tools (default: current directory).
    #[arg(long)]
    workdir: Option<PathBuf>,

    /// Maximum turns in the tool-use loop before stopping.
    #[arg(long, default_value = "50")]
    max_turns: usize,

    /// Temperature for completions (0.0–2.0).
    #[arg(long, default_value = "0.1")]
    temperature: f32,

    /// If set, skip model validation probe (faster startup for trusted endpoints).
    #[arg(long)]
    skip_validation: bool,

    /// Print verbose debug info to stderr.
    #[arg(long, short)]
    verbose: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialise tracing — verbose → DEBUG, default → WARN.
    let level = if args.verbose { "debug" } else { "warn" };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level)),
        )
        .with_writer(std::io::stderr)
        .init();

    // 1. Emit sentinel immediately so TA knows we've started.
    eprintln!("[goal started]");

    // 2. Load goal context.
    let context = load_context(args.context_file.as_deref())?;

    // 3. Load memory snapshot (optional — missing file is fine).
    let memory_snapshot = args
        .memory_path
        .as_deref()
        .and_then(|p| std::fs::read_to_string(p).ok());

    // 4. Build system prompt.
    let system_prompt = build_system_prompt(&context, memory_snapshot.as_deref());

    // 5. Resolve working directory.
    let workdir = args
        .workdir
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    // 6. Initialise memory bridge and tool set.
    let bridge = MemoryBridge::new(args.memory_out.as_deref());
    let tools = ToolSet::new(workdir.clone(), bridge);

    // 7. Create HTTP client.
    let client = OllamaClient::new(&args.base_url, &args.model, args.temperature)?;

    // 8. Validate model (unless skipped).
    if !args.skip_validation {
        validate_model(&client, &args.model).await?;
    }

    // 9. Probe function-calling capability.
    let supports_tools = probe_tool_support(&client, &system_prompt).await;
    if !supports_tools {
        eprintln!(
            "[ta-agent-ollama] WARNING: model '{}' does not appear to support function calling.\n\
             Falling back to Chain-of-Thought mode (best-effort tool extraction from text).\n\
             For full tool-use, use a model with native function-calling support:\n\
               - qwen2.5-coder:7b or higher\n\
               - phi4-mini\n\
               - llama3.1:8b (with Ollama >=0.3.6)\n\
               - Any OpenAI API model",
            args.model
        );
    }

    // 10. Run the main loop.
    if supports_tools {
        run_tool_loop(&client, &system_prompt, &tools, args.max_turns).await?;
    } else {
        run_cot_loop(&client, &system_prompt, &tools, args.max_turns).await?;
    }

    // 11. Flush pending memory writes to TA_MEMORY_OUT.
    tools.flush_memory()?;

    Ok(())
}

/// Load goal context from a file path, returning empty string if absent.
fn load_context(path: Option<&std::path::Path>) -> Result<String> {
    match path {
        None => Ok(String::new()),
        Some(p) => {
            if p.exists() {
                std::fs::read_to_string(p)
                    .with_context(|| format!("Failed to read context file: {}", p.display()))
            } else {
                tracing::warn!(path = %p.display(), "Context file not found — proceeding without context");
                Ok(String::new())
            }
        }
    }
}

/// Build the system prompt, incorporating goal context and memory snapshot.
fn build_system_prompt(context: &str, memory: Option<&str>) -> String {
    let mut prompt = String::new();

    prompt.push_str(
        "You are an autonomous AI agent working inside the Trusted Autonomy framework.\n\
         You have access to tools for reading and writing files, executing shell commands,\n\
         fetching web pages, and persisting memory across sessions.\n\n\
         Work methodically toward the goal. After every tool call, review the result and\n\
         decide on the next action. When the goal is fully complete, stop calling tools.\n\n",
    );

    if !context.is_empty() {
        prompt.push_str("## Goal Context\n\n");
        prompt.push_str(context);
        prompt.push_str("\n\n");
    }

    if let Some(mem) = memory {
        if !mem.is_empty() {
            prompt.push_str(mem);
            prompt.push('\n');
        }
    }

    prompt
}

/// Probe whether the model at /v1/models includes our model name.
async fn validate_model(client: &OllamaClient, model: &str) -> Result<()> {
    match client.list_models().await {
        Ok(models) => {
            // Match on exact name or model:tag prefix.
            let found = models
                .iter()
                .any(|m| m == model || m.starts_with(model) || model.starts_with(m.as_str()));
            if !found {
                eprintln!(
                    "[ta-agent-ollama] WARNING: model '{}' not found in /v1/models.\n\
                     Available models: {}\n\
                     Proceeding anyway — the model may be available via a different endpoint.",
                    model,
                    if models.is_empty() {
                        "(none listed)".to_string()
                    } else {
                        models.join(", ")
                    }
                );
            } else {
                tracing::debug!(model, "Model validated");
            }
        }
        Err(e) => {
            // Endpoint not responding or auth failure — warn but continue.
            eprintln!(
                "[ta-agent-ollama] WARNING: Could not reach /v1/models at {} — {}.\n\
                 Proceeding anyway. If the model is unavailable, tool calls will fail.",
                client.base_url(),
                e
            );
        }
    }
    Ok(())
}

/// Probe whether the model supports function calling by sending a minimal tool call.
/// Returns true if the model responded with a tool_calls field.
async fn probe_tool_support(client: &OllamaClient, system_prompt: &str) -> bool {
    let probe_tool = serde_json::json!({
        "type": "function",
        "function": {
            "name": "probe",
            "description": "Probe tool to test function calling support.",
            "parameters": {
                "type": "object",
                "properties": {
                    "ok": {"type": "boolean", "description": "Always true"}
                },
                "required": []
            }
        }
    });

    match client
        .chat_with_tools(
            system_prompt,
            &[serde_json::json!({"role": "user", "content": "Call the probe tool."})],
            &[probe_tool],
        )
        .await
    {
        Ok(resp) => {
            let has_tool_calls = resp
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("message"))
                .and_then(|m| m.get("tool_calls"))
                .and_then(|tc| tc.as_array())
                .map(|a| !a.is_empty())
                .unwrap_or(false);
            tracing::debug!(has_tool_calls, "Tool support probe result");
            has_tool_calls
        }
        Err(e) => {
            tracing::warn!("Tool support probe failed: {}", e);
            false
        }
    }
}

/// Run the full function-calling tool-use loop.
async fn run_tool_loop(
    client: &OllamaClient,
    system_prompt: &str,
    tools: &ToolSet,
    max_turns: usize,
) -> Result<()> {
    let tool_defs = tools.definitions();
    let mut messages: Vec<serde_json::Value> = Vec::new();

    // Seed the conversation with the user goal from the system prompt (already injected).
    messages.push(serde_json::json!({
        "role": "user",
        "content": "Please complete the goal described in the system prompt. Work methodically and call tools as needed."
    }));

    for turn in 0..max_turns {
        tracing::debug!(turn, "Tool loop turn");

        let response = client
            .chat_with_tools(system_prompt, &messages, &tool_defs)
            .await
            .with_context(|| format!("Chat API call failed on turn {}", turn))?;

        let choice = response
            .get("choices")
            .and_then(|c| c.get(0))
            .cloned()
            .unwrap_or_default();

        let message = choice.get("message").cloned().unwrap_or_default();
        let finish_reason = choice
            .get("finish_reason")
            .and_then(|f| f.as_str())
            .unwrap_or("");

        // Add assistant message to history.
        messages.push(message.clone());

        // Print any assistant text to stdout.
        if let Some(content) = message.get("content").and_then(|c| c.as_str()) {
            if !content.is_empty() {
                println!("{}", content);
            }
        }

        // Check for tool calls.
        let tool_calls = message
            .get("tool_calls")
            .and_then(|tc| tc.as_array())
            .cloned()
            .unwrap_or_default();

        if tool_calls.is_empty() {
            // No tool calls — agent is done.
            tracing::debug!(finish_reason, "No tool calls — agent complete");
            break;
        }

        // Execute each tool call and collect results.
        for call in &tool_calls {
            let call_id = call.get("id").and_then(|i| i.as_str()).unwrap_or("call_0");
            let function = call.get("function").cloned().unwrap_or_default();
            let fn_name = function
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("unknown");
            let fn_args: serde_json::Value = function
                .get("arguments")
                .and_then(|a| {
                    // Arguments may be a string (JSON-encoded) or object.
                    if let Some(s) = a.as_str() {
                        serde_json::from_str(s).ok()
                    } else {
                        Some(a.clone())
                    }
                })
                .unwrap_or_default();

            eprintln!("[ta-agent-ollama] tool: {} {:?}", fn_name, fn_args);

            let result = tools.call(fn_name, &fn_args).await;
            let result_content = match result {
                Ok(v) => v.to_string(),
                Err(e) => serde_json::json!({"error": e.to_string()}).to_string(),
            };

            messages.push(serde_json::json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": result_content
            }));
        }

        if finish_reason == "stop" {
            break;
        }
    }

    Ok(())
}

/// Run Chain-of-Thought fallback loop: extract tool calls from plain text output.
async fn run_cot_loop(
    client: &OllamaClient,
    system_prompt: &str,
    tools: &ToolSet,
    max_turns: usize,
) -> Result<()> {
    let tool_descriptions = tools.text_descriptions();
    let cot_system = format!(
        "{}\n\n## Available Tools (text format — include tool calls in your response)\n\n{}\n\n\
         To call a tool, output a line starting with TOOL_CALL: followed by JSON:\n\
         TOOL_CALL: {{\"name\": \"file_read\", \"args\": {{\"path\": \"README.md\"}}}}\n\
         The result will be provided and you can continue reasoning.",
        system_prompt, tool_descriptions
    );

    let mut history = String::new();

    for turn in 0..max_turns {
        tracing::debug!(turn, "CoT loop turn");

        let prompt = if history.is_empty() {
            "Please complete the goal described in the system prompt.".to_string()
        } else {
            format!("Previous interaction:\n{}\n\nContinue:", history)
        };

        let response = client
            .chat_simple(&cot_system, &prompt)
            .await
            .with_context(|| format!("Chat API call failed on CoT turn {}", turn))?;

        println!("{}", response);
        history.push_str(&format!("Assistant: {}\n\n", response));

        // Extract and execute any tool calls from the response.
        let mut called_any = false;
        for line in response.lines() {
            let trimmed = line.trim();
            if let Some(json_str) = trimmed.strip_prefix("TOOL_CALL:") {
                let json_str = json_str.trim();
                if let Ok(call) = serde_json::from_str::<serde_json::Value>(json_str) {
                    let name = call
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("unknown");
                    let args = call.get("args").cloned().unwrap_or_default();
                    eprintln!("[ta-agent-ollama] cot-tool: {} {:?}", name, args);
                    let result = tools.call(name, &args).await;
                    let result_str = match result {
                        Ok(v) => v.to_string(),
                        Err(e) => format!("Error: {}", e),
                    };
                    history.push_str(&format!("TOOL_RESULT: {}\n\n", result_str));
                    called_any = true;
                }
            }
        }

        // If no tool calls, the agent is done.
        if !called_any {
            break;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_prompt_includes_context() {
        let ctx = "## Goal Context\nFix the bug.";
        let prompt = build_system_prompt(ctx, None);
        assert!(prompt.contains("Fix the bug."));
        assert!(prompt.contains("autonomous AI agent"));
    }

    #[test]
    fn system_prompt_includes_memory() {
        let prompt = build_system_prompt("", Some("## Memory\n- Key: Value"));
        assert!(prompt.contains("Memory"));
        assert!(prompt.contains("Key: Value"));
    }

    #[test]
    fn load_context_missing_file_ok() {
        let result = load_context(Some(std::path::Path::new("/nonexistent/path/ctx.md")));
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn load_context_none_returns_empty() {
        let result = load_context(None);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn load_context_reads_file() {
        let dir = tempfile::tempdir().unwrap();
        let ctx_path = dir.path().join("context.md");
        std::fs::write(&ctx_path, "# Goal\nDo something.").unwrap();
        let result = load_context(Some(&ctx_path)).unwrap();
        assert_eq!(result, "# Goal\nDo something.");
    }
}
