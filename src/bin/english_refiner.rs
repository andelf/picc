//! English grammar/spelling correction hook for Claude Code UserPromptSubmit.
//!
//! Uses Kimi API (OpenAI-compatible) for fast, direct HTTP correction.
//!
//! Usage:
//!   echo '{"user_message":"I has went to the store"}' | english-refiner
//!   KIMI_API_KEY=sk-... english-refiner

use std::io::Write;
use std::time::Duration;

use clap::Parser;
use serde::{Deserialize, Serialize};

#[derive(Parser)]
#[command(name = "english-refiner", about = "English grammar/spelling correction hook")]
struct Cli {
    /// Output raw JSON instead of human-friendly format
    #[arg(long)]
    json: bool,
}

#[derive(Deserialize)]
struct HookInput {
    user_message: String,
}

#[derive(Deserialize)]
struct RefinedOutput {
    refined: String,
    changes: Vec<String>,
}

#[derive(Serialize)]
struct FinalOutput {
    original: String,
    refined: String,
    changes: Vec<String>,
}

// --- Kimi API types (OpenAI-compatible) ---

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f64,
}

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    content: String,
}

fn is_english(s: &str) -> bool {
    if s.contains('\n') {
        return false;
    }
    let total = s.chars().count();
    if total == 0 {
        return false;
    }
    let ascii_letters = s.chars().filter(|c| c.is_ascii_alphabetic()).count();
    let ratio = ascii_letters as f64 / total as f64;
    let word_count = s.split_whitespace().count();
    ratio > 0.7 && word_count > 3
}

fn extract_json(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let end = s.rfind('}')? + 1;
    Some(&s[start..end])
}

const SYSTEM_PROMPT: &str = "You are an English proofreader. Fix grammar and spelling in the user's sentence.\n\
Return ONLY valid JSON: {\"refined\": \"<corrected sentence>\", \"changes\": [\"<brief description of each change>\"]}\n\
If no changes needed, return: {\"refined\": \"<original>\", \"changes\": []}";

fn call_kimi(user_input: &str) -> Result<RefinedOutput, String> {
    let api_key = std::env::var("KIMI_API_KEY")
        .map_err(|_| "KIMI_API_KEY not set".to_string())?;

    let request = ChatRequest {
        model: "kimi-k2-0905-preview".to_string(),
        messages: vec![
            ChatMessage {
                role: "system".to_string(),
                content: SYSTEM_PROMPT.to_string(),
            },
            ChatMessage {
                role: "user".to_string(),
                content: user_input.to_string(),
            },
        ],
        temperature: 0.3,
    };

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("client error: {e}"))?;

    let resp = client
        .post("https://api.moonshot.cn/v1/chat/completions")
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&request)
        .send()
        .map_err(|e| format!("HTTP error: {e}"))?;

    let status = resp.status();
    let body = resp.text().map_err(|e| format!("read error: {e}"))?;

    if !status.is_success() {
        return Err(format!("API {status}: {body}"));
    }

    let parsed: ChatResponse =
        serde_json::from_str(&body).map_err(|e| format!("response parse: {e}"))?;

    let content = parsed
        .choices
        .first()
        .map(|c| c.message.content.trim().to_string())
        .ok_or("no choices")?;

    let json_str = extract_json(&content).ok_or("no JSON in response")?;
    serde_json::from_str(json_str).map_err(|e| format!("JSON parse: {e}\nRaw: {content}"))
}

fn main() {
    let cli = Cli::parse();

    let input: HookInput = match serde_json::from_reader(std::io::stdin()) {
        Ok(v) => v,
        Err(_) => return,
    };

    if !is_english(&input.user_message) {
        return;
    }

    match call_kimi(&input.user_message) {
        Ok(refined) if refined.refined != input.user_message => {
            let output = FinalOutput {
                original: input.user_message,
                refined: refined.refined,
                changes: refined.changes,
            };
            let json = serde_json::to_string(&output).unwrap();
            if cli.json {
                eprintln!("{json}");
            } else {
                eprintln!("[english-refiner] \"{}\" → \"{}\"", output.original, output.refined);
                for change in &output.changes {
                    eprintln!("  • {change}");
                }
            }
            if let Some(home) = std::env::var_os("HOME") {
                let home = std::path::Path::new(&home);
                // Append to log
                let log_path = home.join(".english-refiner.log");
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(log_path)
                {
                    let _ = writeln!(f, "{json}");
                }
                // Write latest for statusLine display
                let latest_path = home.join(".english-refiner-latest");
                let _ = std::fs::write(latest_path, format!("\"{}\" → \"{}\"", output.original, output.refined));
            }
        }
        Ok(_) => {} // no changes needed
        Err(e) => eprintln!("[english-refiner] error: {e}"),
    }
}
