use eyre::Result;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;

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

pub fn load_config() -> Result<Config> {
    let config_path = get_config_path()?;
    if !config_path.exists() {
        let default_config = Config::default();
        let toml_string = toml::to_string(&default_config)?;
        let mut file = File::create(&config_path)?;
        file.write_all(toml_string.as_bytes())?;
        println!("Created default config at {:?}", config_path);
        return Ok(default_config);
    }

    let toml_string = fs::read_to_string(config_path)?;
    let config: Config = toml::from_str(&toml_string)?;
    Ok(config)
}
