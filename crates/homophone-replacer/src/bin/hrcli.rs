use std::path::PathBuf;

use clap::{Parser, Subcommand};
use homophone_replacer::{
    build_rules_from_terms, replace_text, replace_text_from_files, CompiledReplacer, Lexicon,
    ReplaceRuleSet, ReplacerConfig,
};

fn default_lexicon() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/lexicon.txt")
}

fn default_rules() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/common-mistakes.rules.txt")
}

#[derive(Parser, Debug)]
#[command(name = "hrcli")]
#[command(about = "Compile and test homophone replacement rules")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Replace {
        #[arg(long, default_value_os_t = default_lexicon())]
        lexicon: PathBuf,
        #[arg(long, default_value_os_t = default_rules())]
        rules: PathBuf,
        #[arg(long)]
        terms: Option<PathBuf>,
        #[arg(long, default_value_t = 10)]
        max_phrase_len: usize,
        #[arg(long)]
        text: String,
    },
    Compile {
        #[arg(long, default_value_os_t = default_lexicon())]
        lexicon: PathBuf,
        #[arg(long, default_value_os_t = default_rules())]
        rules: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    ReplaceCompiled {
        #[arg(long)]
        compiled: PathBuf,
        #[arg(long, default_value_t = 10)]
        max_phrase_len: usize,
        #[arg(long)]
        text: String,
    },
    Inspect {
        #[arg(long, default_value_os_t = default_lexicon())]
        lexicon: PathBuf,
        #[arg(long, default_value_os_t = default_rules())]
        rules: PathBuf,
        #[arg(long)]
        terms: Option<PathBuf>,
        #[arg(long)]
        text: String,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Command::Replace {
            lexicon,
            rules,
            terms,
            max_phrase_len,
            text,
        } => {
            let config = ReplacerConfig { max_phrase_len };
            let output = match terms {
                Some(terms) => {
                    let lexicon = Lexicon::from_path(lexicon)?;
                    let terms = load_terms(&terms)?;
                    let rules = build_rules_from_terms(&lexicon, terms.iter().map(String::as_str));
                    replace_text(&lexicon, &rules, &config, &text)
                }
                None => replace_text_from_files(lexicon, rules, &config, &text)?,
            };
            println!("{output}");
        }
        Command::Compile {
            lexicon,
            rules,
            output,
        } => {
            let compiled = CompiledReplacer::compile_from_files(lexicon, rules)?;
            compiled.save_to_path(&output)?;
            println!("{}", output.display());
        }
        Command::ReplaceCompiled {
            compiled,
            max_phrase_len,
            text,
        } => {
            let compiled = CompiledReplacer::load_from_path(compiled)?;
            let config = ReplacerConfig { max_phrase_len };
            println!("{}", compiled.replace(&config, &text));
        }
        Command::Inspect {
            lexicon,
            rules,
            terms,
            text,
        } => {
            let lexicon = Lexicon::from_path(lexicon)?;
            let rules = match terms {
                Some(terms) => {
                    let terms = load_terms(&terms)?;
                    build_rules_from_terms(&lexicon, terms.iter().map(String::as_str))
                }
                None => ReplaceRuleSet::from_path(rules)?,
            };
            let config = ReplacerConfig { max_phrase_len: 10 };
            println!("input: {text}");
            println!("output: {}", replace_text(&lexicon, &rules, &config, &text));
        }
    }

    Ok(())
}

fn load_terms(path: &PathBuf) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(path)?;
    let terms = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    Ok(terms)
}
