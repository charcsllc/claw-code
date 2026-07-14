use std::io::{self, IsTerminal, Write};

use runtime::{
    load_user_settings_field, save_user_provider_settings, save_user_settings_field, ConfigLoader,
    RuntimeProviderConfig,
};

const PROVIDERS: &[(&str, &str, &str)] = &[
    ("1", "Anthropic", "anthropic"),
    ("2", "xAI / Grok", "xai"),
    ("3", "OpenAI", "openai"),
    ("4", "DashScope (Qwen)", "dashscope"),
    ("5", "Zhipu / Z.ai (GLM, coding plan)", "zhipu"),
    ("6", "Moonshot / Kimi", "kimi"),
    ("7", "DeepSeek", "deepseek"),
    ("8", "Custom (OpenAI-compat)", "openai"),
];

const PROVIDER_MODELS: &[(&str, &[&str])] = &[
    ("anthropic", &["opus", "sonnet", "haiku"]),
    ("xai", &["grok", "grok-mini", "grok-2"]),
    ("openai", &["gpt-4.1", "gpt-4.1-mini", "gpt-4.1-nano"]),
    ("dashscope", &["qwen-plus", "qwen-max", "qwen3-coder"]),
    ("zhipu", &["glm-4.6", "glm-4.5-air"]),
    ("kimi", &["kimi-k2-0905-preview", "kimi-k2-turbo-preview"]),
    ("deepseek", &["deepseek-chat", "deepseek-reasoner"]),
];

const DEFAULT_BASE_URLS: &[(&str, &str)] = &[
    ("anthropic", "https://api.anthropic.com"),
    ("xai", "https://api.x.ai/v1"),
    ("openai", "https://api.openai.com/v1"),
    (
        "dashscope",
        "https://dashscope.aliyuncs.com/compatible-mode/v1",
    ),
    // Anthropic-protocol endpoints of the Chinese coding providers: their
    // tokens ride ANTHROPIC_AUTH_TOKEN + ANTHROPIC_BASE_URL.
    ("zhipu", "https://api.z.ai/api/anthropic"),
    ("kimi", "https://api.moonshot.ai/anthropic"),
    ("deepseek", "https://api.deepseek.com"),
];

const API_KEY_ENV_VARS: &[(&str, &str)] = &[
    ("anthropic", "ANTHROPIC_API_KEY"),
    ("xai", "XAI_API_KEY"),
    ("openai", "OPENAI_API_KEY"),
    ("dashscope", "DASHSCOPE_API_KEY"),
    ("zhipu", "ANTHROPIC_AUTH_TOKEN"),
    ("kimi", "ANTHROPIC_AUTH_TOKEN"),
    ("deepseek", "OPENAI_API_KEY"),
];

pub fn run_setup_wizard() -> Result<(), Box<dyn std::error::Error>> {
    if !io::stdin().is_terminal() {
        return Err("setup wizard requires an interactive terminal".into());
    }

    let current = load_current_provider_config();

    println!();
    println!("  \x1b[1mClaw Code Setup Wizard\x1b[0m");
    println!("  Configure your provider, API key, and model.");
    println!("  Press Enter to keep current value.\n");

    let kind = prompt_provider(&current)?;
    let api_key = prompt_api_key(&kind, &current)?;
    let base_url = prompt_base_url(&kind, &current)?;
    let model = prompt_model(&kind, &current)?;
    let fast_model = prompt_fast_model(&current, model.as_deref())?;

    save_user_provider_settings(&kind, &api_key, base_url.as_deref(), model.as_deref())?;

    if let Some(fast) = &fast_model {
        save_user_settings_field("subagentModel", fast)?;
    }

    println!();
    println!("  \x1b[32mProvider saved to ~/.claw/settings.json\x1b[0m");
    println!(
        "  Run \x1b[1m/model {}\x1b[0m or restart claw to activate.",
        model.as_deref().unwrap_or(&kind)
    );
    println!();

    Ok(())
}

fn load_current_provider_config() -> RuntimeProviderConfig {
    let cwd = std::env::current_dir().unwrap_or_default();
    ConfigLoader::default_for(&cwd)
        .load()
        .map(|c| c.provider().clone())
        .unwrap_or_default()
}

fn prompt_provider(current: &RuntimeProviderConfig) -> Result<String, Box<dyn std::error::Error>> {
    let current_kind = current.kind().unwrap_or("anthropic");
    println!("  \x1b[1mProvider\x1b[0m");
    for (num, label, kind) in PROVIDERS {
        let marker = if *kind == current_kind {
            " (current)"
        } else {
            ""
        };
        println!("    [{num}] {label}{marker}");
    }
    let default = PROVIDERS
        .iter()
        .position(|(_, _, k)| *k == current_kind)
        .map_or_else(|| "1".to_string(), |i| (i + 1).to_string());

    let input = read_line(&format!("  Select provider [{default}]: "))?;
    let choice = if input.trim().is_empty() {
        default
    } else {
        input.trim().to_string()
    };

    let kind = PROVIDERS
        .iter()
        .find(|(num, _, _)| *num == choice)
        .map(|(_, _, kind)| *kind)
        .ok_or_else(|| format!("invalid provider choice: {choice}"))?;

    Ok(kind.to_string())
}

fn prompt_api_key(
    kind: &str,
    current: &RuntimeProviderConfig,
) -> Result<String, Box<dyn std::error::Error>> {
    let env_var = API_KEY_ENV_VARS
        .iter()
        .find(|(k, _)| *k == kind)
        .map_or("API_KEY", |(_, v)| *v);

    let current_key = current.api_key();
    let hint = match current_key {
        Some(key) if !key.is_empty() => {
            // Char-based, not byte-based: a multibyte key (e.g. one with a
            // `€`) would panic on a mid-codepoint byte slice.
            let last4: String = {
                let chars: Vec<char> = key.chars().collect();
                let start = chars.len().saturating_sub(4);
                chars[start..].iter().collect()
            };
            let masked = if key.chars().count() > 4 {
                format!("****{last4}")
            } else {
                "****".to_string()
            };
            format!("[{masked}]")
        }
        _ => "(none)".to_string(),
    };

    // Check if env var is already set
    let env_set = std::env::var(env_var).ok().is_some_and(|v| !v.is_empty());
    if env_set {
        println!("  {env_var} is set in environment (will take priority over stored key)");
    }

    let input = read_line(&format!("  API key ({env_var}) {hint}: "))?;
    let key = if input.trim().is_empty() {
        current_key.unwrap_or("").to_string()
    } else {
        input.trim().to_string()
    };

    if key.is_empty() && !env_set {
        eprintln!(
            "  \x1b[33mWarning: no API key configured. Set {env_var} or re-run setup.\x1b[0m"
        );
    }

    Ok(key)
}

fn prompt_base_url(
    kind: &str,
    current: &RuntimeProviderConfig,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let default_url = DEFAULT_BASE_URLS
        .iter()
        .find(|(k, _)| *k == kind)
        .map_or("", |(_, v)| *v);

    let current_url = current.base_url().unwrap_or(default_url);
    let display = if current_url.is_empty() {
        default_url.to_string()
    } else {
        current_url.to_string()
    };

    // Check if the relevant env var is already set
    let env_var = match kind {
        "anthropic" | "zhipu" | "kimi" => "ANTHROPIC_BASE_URL",
        "xai" => "XAI_BASE_URL",
        "openai" | "deepseek" => "OPENAI_BASE_URL",
        "dashscope" => "DASHSCOPE_BASE_URL",
        _ => "BASE_URL",
    };
    let env_set = std::env::var(env_var).ok().is_some_and(|v| !v.is_empty());
    if env_set {
        println!("  {env_var} is set in environment (will take priority over stored URL)");
    }

    let input = read_line(&format!("  Base URL [{display}]: "))?;
    if input.trim().is_empty() {
        if current_url == default_url || current_url.is_empty() {
            Ok(None)
        } else {
            Ok(Some(current_url.to_string()))
        }
    } else {
        Ok(Some(input.trim().to_string()))
    }
}

fn prompt_model(
    kind: &str,
    current: &RuntimeProviderConfig,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let empty: &[&str] = &[];
    let aliases = PROVIDER_MODELS
        .iter()
        .find(|(k, _)| *k == kind)
        .map_or(empty, |(_, models)| *models);

    let current_model = current
        .model()
        .unwrap_or(aliases.first().copied().unwrap_or(""));

    println!("  \x1b[1mModel\x1b[0m");
    if !aliases.is_empty() {
        println!("    Common: {}", aliases.join(", "));
    }
    println!("    Or enter any model name (e.g. openai/gpt-4.1-mini for custom routing)");

    let input = read_line(&format!("  Model [{current_model}]: "))?;
    if input.trim().is_empty() {
        if current_model.is_empty() {
            Ok(None)
        } else {
            Ok(Some(current_model.to_string()))
        }
    } else {
        Ok(Some(input.trim().to_string()))
    }
}

fn prompt_fast_model(
    current: &RuntimeProviderConfig,
    main_model: Option<&str>,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    println!();
    println!("  \x1b[1mFast Model (for Agent subtasks)\x1b[0m");
    println!("    A smaller/cheaper model used by the Agent tool when spawning");
    println!("    Explore, Plan, or Verification sub-agents. This saves tokens");
    println!("    by using a fast model for information-gathering tasks.");
    println!("    Press Enter to skip (agents will use your main model).");

    let current_fast = load_user_settings_field("subagentModel");
    let default_hint = current_fast.as_deref().or(main_model).unwrap_or("");

    let input = read_line(&format!(
        "  Fast model [{}]: ",
        if default_hint.is_empty() {
            "same as main"
        } else {
            default_hint
        }
    ))?;
    if input.trim().is_empty() {
        Ok(current_fast)
    } else {
        Ok(Some(input.trim().to_string()))
    }
}

fn read_line(prompt: &str) -> Result<String, Box<dyn std::error::Error>> {
    let mut stdout = io::stdout();
    write!(stdout, "{prompt}")?;
    stdout.flush()?;
    let mut buffer = String::new();
    io::stdin().read_line(&mut buffer)?;
    Ok(buffer)
}
