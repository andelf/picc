use std::io::Write;
use std::time::Duration;

use clap::Parser;
use serde::{Deserialize, Serialize};

#[derive(Parser)]
#[command(name = "english-refiner", about = "English grammar/spelling correction hook")]
struct Cli {
    /// AI agent to use (can be specified multiple times)
    #[arg(long = "agent", default_values_t = vec!["claude".to_string()])]
    agents: Vec<String>,

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

/// Returns (command, args_before_prompt) for each agent.
fn agent_args(name: &str) -> Option<(&str, Vec<&str>)> {
    match name {
        "claude" => Some(("claude", vec!["-p", "--model", "claude-haiku-4-5-20251001"])),
        "codex" => Some(("codex", vec!["exec", "--model", "o4-mini"])),
        "gemini" => Some(("gemini", vec!["-p"])),
        "cursor-agent" => Some(("cursor-agent", vec!["-p"])),
        _ => None,
    }
}

fn extract_json(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let end = s.rfind('}')? + 1;
    Some(&s[start..end])
}

async fn run_agent(agent: &str, prompt: &str) -> Option<RefinedOutput> {
    let (cmd, args) = agent_args(agent)?;
    let which = tokio::process::Command::new("which")
        .arg(cmd)
        .output()
        .await
        .ok()?;
    if !which.status.success() {
        return None;
    }
    let output = tokio::process::Command::new(cmd)
        .args(&args)
        .arg(prompt)
        .env_remove("CLAUDECODE")
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json_str = extract_json(&stdout)?;
    serde_json::from_str(json_str).ok()
}

fn build_prompt(input: &str) -> String {
    format!(
        "You are an English proofreader. Fix grammar and spelling in the sentence below.\n\
         Return ONLY valid JSON: {{\"refined\": \"<corrected sentence>\", \"changes\": [\"<brief description of each change>\"]}}\n\
         If no changes needed, return: {{\"refined\": \"<original>\", \"changes\": []}}\n\n\
         Sentence: {input}"
    )
}

async fn race_first_some(agents: &[String], prompt: &str) -> Option<RefinedOutput> {
    use tokio::sync::mpsc;

    let (tx, mut rx) = mpsc::channel(agents.len());

    for agent in agents {
        let tx = tx.clone();
        let agent = agent.clone();
        let prompt = prompt.to_string();
        tokio::spawn(async move {
            if let Some(result) = run_agent(&agent, &prompt).await {
                let _ = tx.send(result).await;
            }
        });
    }
    drop(tx);

    rx.recv().await
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let input: HookInput = match serde_json::from_reader(std::io::stdin()) {
        Ok(v) => v,
        Err(_) => return,
    };

    if !is_english(&input.user_message) {
        return;
    }

    let prompt = build_prompt(&input.user_message);

    let result = tokio::time::timeout(Duration::from_secs(30), race_first_some(&cli.agents, &prompt)).await;

    if let Ok(Some(refined)) = result {
        if refined.refined != input.user_message {
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
            // Append to global log (always JSON)
            if let Some(home) = std::env::var_os("HOME") {
                let log_path = std::path::Path::new(&home).join(".english-refiner.log");
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(log_path)
                {
                    let _ = writeln!(f, "{json}");
                }
            }
        }
    }
}
