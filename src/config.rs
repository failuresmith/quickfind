use eyre::Result;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

fn default_watch_pending_ram_cap_mb() -> usize {
    200
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Config {
    pub include: Vec<String>,
    pub ignore: Vec<String>,
    pub depth: usize,
    pub highlight_color: Option<String>,
    pub editor: Option<String>,
    #[serde(default = "default_watch_pending_ram_cap_mb")]
    pub watch_pending_ram_cap_mb: usize,
}

impl Default for Config {
    fn default() -> Self {
        let home_dir_opt = home::home_dir();
        let mut default_include = vec![];

        if let Some(home_dir) = home_dir_opt {
            // Add common directories relative to home
            let common_dirs = vec!["Documents", "Projects", "Code", "Desktop"];
            for dir in common_dirs {
                // Construct the full path and convert to string
                if let Some(path_str) = home_dir.join(dir).to_str() {
                    default_include.push(path_str.to_string());
                }
            }
        }

        Self {
            include: default_include,
            ignore: vec![
                "**/.*".to_string(),
                "**/.git".to_string(),
                "**/node_modules/**".to_string(),
                "**/bower_components/**".to_string(),
                "**/.venv/**".to_string(),
                "**/venv/**".to_string(),
                "**/__pycache__/**".to_string(),
                "**/.mypy_cache/**".to_string(),
                "**/.pytest_cache/**".to_string(),
                "**/.tox/**".to_string(),
                "**/.eggs/**".to_string(),
                "**/*.egg-info/**".to_string(),
                "**/target/**".to_string(),
                "**/.cargo/**".to_string(),
                "**/bin/**".to_string(),
                "**/pkg/**".to_string(),
                "**/build/**".to_string(),
                "**/out/**".to_string(),
                "**/.gradle/**".to_string(),
                "**/CMakeFiles/**".to_string(),
                "**/cmake-build-*/**".to_string(),
                "**/.env/**".to_string(),
                "**/.direnv/**".to_string(),
                "**/.cache/**".to_string(),
                "**/.local/**".to_string(),
                "**/.uv/**".to_string(),
                "**/.yarn/**".to_string(),
                "**/.pnpm-store/**".to_string(),
                "**/.next/**".to_string(),
                "**/dist/**".to_string(),
                "**/coverage/**".to_string(),
            ],
            depth: 10,
            highlight_color: None,
            editor: None, // vi, vim, nvim, subl, code, etc.
            watch_pending_ram_cap_mb: default_watch_pending_ram_cap_mb(),
        }
    }
}

pub fn get_config_path() -> Result<PathBuf> {
    let home_dir = home::home_dir().ok_or_else(|| eyre::eyre!("Could not find home directory"))?;
    let config_dir = home_dir.join(".quickfind");
    fs::create_dir_all(&config_dir)?;
    Ok(config_dir.join("conf.toml"))
}

fn read_config_from_path(config_path: &Path) -> Result<Config> {
    let toml_string = fs::read_to_string(config_path)?;
    let config: Config = toml::from_str(&toml_string)?;
    Ok(config)
}

pub fn save_config(config: &Config) -> Result<()> {
    let config_path = get_config_path()?;
    let toml_string = toml::to_string(config)?;
    let mut file = File::create(config_path)?;
    file.write_all(toml_string.as_bytes())?;
    Ok(())
}

fn prompt_line(prompt: &str) -> Result<String> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut buf = String::new();
    io::stdin().read_line(&mut buf)?;
    Ok(buf.trim().to_string())
}

fn normalize_include_input(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(String::new());
    }

    let mut path = if trimmed == "~" || trimmed.starts_with("~/") {
        let home_dir = home::home_dir().ok_or_else(|| eyre::eyre!("Could not find home directory"))?;
        if trimmed == "~" {
            home_dir
        } else {
            home_dir.join(trimmed.trim_start_matches("~/"))
        }
    } else {
        PathBuf::from(trimmed)
    };

    if !path.is_absolute() {
        path = std::env::current_dir()?.join(path);
    }

    let normalized = fs::canonicalize(&path).unwrap_or(path);
    if !normalized.exists() {
        eprintln!("warning: path does not currently exist, adding anyway: {}", normalized.display());
    }

    Ok(normalized.to_string_lossy().to_string())
}

pub fn run_init_onboarding() -> Result<()> {
    let config_path = get_config_path()?;
    let mut config = if config_path.exists() {
        read_config_from_path(&config_path)?
    } else {
        Config::default()
    };

    println!("quickfind onboarding");
    println!("------------------");
    if !config.include.is_empty() {
        println!("Current indexed locations:");
        for location in &config.include {
            println!("  - {location}");
        }
    }

    println!();
    println!("Enter locations to index (one per line). Press ENTER on an empty line to finish.");
    println!("You can use absolute paths, relative paths, or ~/path.");

    let mut include = Vec::<String>::new();
    loop {
        let input = prompt_line("include> ")?;
        if input.is_empty() {
            break;
        }

        let normalized = normalize_include_input(&input)?;
        if normalized.is_empty() {
            continue;
        }

        if !include.contains(&normalized) {
            include.push(normalized);
        }
    }

    if include.is_empty() {
        if config.include.is_empty() {
            let fallback = std::env::current_dir()?.to_string_lossy().to_string();
            println!("No locations provided; using current directory fallback: {fallback}");
            include.push(fallback);
            config.include = include;
        } else {
            println!("No new locations provided; keeping existing config locations.");
        }
    } else {
        config.include = include;
    }

    let cap_input = prompt_line(&format!(
        "Watcher RAM cap in MB [{}]: ",
        config.watch_pending_ram_cap_mb
    ))?;
    if !cap_input.is_empty() {
        match cap_input.parse::<usize>() {
            Ok(value) if value > 0 => config.watch_pending_ram_cap_mb = value,
            _ => eprintln!(
                "invalid RAM cap input, keeping previous value: {} MB",
                config.watch_pending_ram_cap_mb
            ),
        }
    }

    save_config(&config)?;

    println!();
    println!("Config saved to {}", config_path.display());
    println!("Next steps:");
    println!("  1) quickfind --index");
    println!("  2) optionally run quickfind --setup for full setup + daemon prompt");
    println!("  3) quickfind <query>");

    Ok(())
}

pub fn load_config() -> Result<Config> {
    let config_path = get_config_path()?;
    if !config_path.exists() {
        let default_config = Config::default();
        save_config(&default_config)?;
        println!("Created default config at {:?}", config_path);
        return Ok(default_config);
    }

    read_config_from_path(&config_path)
}
