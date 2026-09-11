use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

use crate::error::{AgentWikiError, Result};

const DEFAULT_WIKI_ROOT: &str = "~/AgentWiki";

const CONFIG_FILE_NAME: &str = "config.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppConfig {
    pub wiki_root: Utf8PathBuf,
    pub embedding_model: Option<String>,
    pub projection_dir: Utf8PathBuf,
    pub config_file: Option<Utf8PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RawConfig {
    #[serde(default = "default_wiki_root")]
    wiki_root: Utf8PathBuf,
    #[serde(default)]
    embedding_model: Option<String>,
}

impl AppConfig {
    pub fn load() -> Result<AppConfig> {
        let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
        let dir = Utf8PathBuf::from_path_buf(home)
            .map_err(|p| AgentWikiError::Config(format!("home is not valid UTF-8: {p:?}")))?
            .join(".agentwiki");
        Self::load_from(&dir)
    }

    pub fn load_from(dir: &Utf8Path) -> Result<AppConfig> {
        let dir = absolute_of(dir);
        let path = dir.join(CONFIG_FILE_NAME);

        let (raw, config_file) = if path.exists() {
            let text = std::fs::read_to_string(&path).map_err(|e| AgentWikiError::Io {
                path: path.clone(),
                source: e,
            })?;
            let raw: RawConfig = serde_json::from_str(&text)
                .map_err(|e| AgentWikiError::Config(format!("{path}: {e}")))?;
            (raw, Some(path.clone()))
        } else {
            let defaults = RawConfig {
                wiki_root: default_wiki_root(),
                embedding_model: None,
            };
            std::fs::create_dir_all(&dir).map_err(|e| AgentWikiError::Io {
                path: dir.clone(),
                source: e,
            })?;
            let json = serde_json::to_string_pretty(&defaults)
                .expect("serializing the default config cannot fail");
            std::fs::write(&path, json).map_err(|e| AgentWikiError::Io {
                path: path.clone(),
                source: e,
            })?;
            (defaults, None)
        };

        validate_config(&raw, &path)?;

        Ok(AppConfig {
            wiki_root: resolve_path(&dir, &raw.wiki_root),
            embedding_model: raw.embedding_model,
            projection_dir: dir,
            config_file,
        })
    }
}

fn validate_config(raw: &RawConfig, path: &Utf8Path) -> Result<()> {
    if let Err(e) = validate_nonempty_path(&raw.wiki_root) {
        return Err(AgentWikiError::Config(format!(
            "{path}: invalid `wiki_root`: {}",
            e.code
        )));
    }
    if let Err(e) = validate_model(&raw.embedding_model) {
        return Err(AgentWikiError::Config(format!(
            "{path}: invalid `embedding_model`: {}",
            e.code
        )));
    }
    Ok(())
}

fn default_wiki_root() -> Utf8PathBuf {
    Utf8PathBuf::from(DEFAULT_WIKI_ROOT)
}

fn validate_nonempty_path(p: &Utf8PathBuf) -> std::result::Result<(), validator::ValidationError> {
    if p.as_str().trim().is_empty() {
        return Err(validator::ValidationError::new("empty_path"));
    }
    Ok(())
}

fn validate_model(m: &Option<String>) -> std::result::Result<(), validator::ValidationError> {
    if m.as_deref().is_some_and(|s| s.trim().is_empty()) {
        return Err(validator::ValidationError::new("empty_model"));
    }
    Ok(())
}

fn resolve_path(base_dir: &Utf8Path, value: &Utf8Path) -> Utf8PathBuf {
    if let Some(rest) = value.as_str().strip_prefix('~')
        && let Some(home) = dirs::home_dir()
    {
        let home = Utf8PathBuf::from_path_buf(home).unwrap_or_else(|_| base_dir.to_path_buf());
        let rest = rest.trim_start_matches('/');
        return if rest.is_empty() {
            home
        } else {
            home.join(rest)
        };
    }
    if value.is_absolute() {
        value.to_path_buf()
    } else {
        base_dir.join(value)
    }
}

fn absolute_of(p: &Utf8Path) -> Utf8PathBuf {
    if p.is_absolute() {
        return p.to_path_buf();
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    Utf8PathBuf::from_path_buf(cwd)
        .map(|c| c.join(p))
        .unwrap_or_else(|_| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn tmp_dir() -> (tempfile::TempDir, Utf8PathBuf) {
        let t = tempdir().expect("tempdir");
        let p = Utf8PathBuf::from_path_buf(t.path().to_path_buf()).expect("utf8 tempdir");
        (t, p)
    }

    fn write_config(dir: &Utf8Path, json: &str) {
        std::fs::write(dir.join(CONFIG_FILE_NAME), json).expect("write config");
    }

    fn home() -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(dirs::home_dir().expect("home exists")).expect("utf8 home")
    }

    #[test]
    fn first_run_seeds_default_config() {
        let (_t, dir) = tmp_dir();
        let cfg = AppConfig::load_from(&dir).expect("load");

        let path = dir.join(CONFIG_FILE_NAME);
        assert!(path.exists(), "default config file must be created");
        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(on_disk["wiki_root"], "~/AgentWiki");
        assert_eq!(on_disk["embedding_model"], serde_json::Value::Null);

        assert_eq!(cfg.wiki_root, home().join("AgentWiki"));
        assert_eq!(cfg.embedding_model, None);
        assert_eq!(cfg.projection_dir, dir);
        assert_eq!(cfg.config_file, None, "first run just created the file");
    }

    #[test]
    fn tilde_expands_to_home_directory() {
        let (_t, dir) = tmp_dir();
        write_config(&dir, r#"{ "wiki_root": "~/Notes" }"#);
        let cfg = AppConfig::load_from(&dir).expect("load");
        assert_eq!(cfg.wiki_root, home().join("Notes"));
    }

    #[test]
    fn relative_path_anchors_at_config_dir() {
        let (_t, dir) = tmp_dir();
        write_config(&dir, r#"{ "wiki_root": "wikis/main" }"#);
        let cfg = AppConfig::load_from(&dir).expect("load");
        assert_eq!(cfg.wiki_root, dir.join("wikis/main"));
    }

    #[test]
    fn absolute_path_passes_through() {
        let (_t, dir) = tmp_dir();
        write_config(&dir, r#"{ "wiki_root": "/tmp/some/wiki" }"#);
        let cfg = AppConfig::load_from(&dir).expect("load");
        assert_eq!(cfg.wiki_root, Utf8PathBuf::from("/tmp/some/wiki"));
    }

    #[test]
    fn legacy_document_root_is_ignored() {
        let (_t, dir) = tmp_dir();
        write_config(&dir, r#"{ "document_root": "/somewhere/else" }"#);
        let cfg = AppConfig::load_from(&dir).expect("load");
        assert_eq!(cfg.wiki_root, home().join("AgentWiki"));
    }

    #[test]
    fn embedding_model_is_optional() {
        let (_t, dir) = tmp_dir();

        write_config(&dir, r#"{ "wiki_root": "~/W" }"#);
        assert_eq!(AppConfig::load_from(&dir).unwrap().embedding_model, None);

        write_config(&dir, r#"{ "wiki_root": "~/W", "embedding_model": null }"#);
        assert_eq!(AppConfig::load_from(&dir).unwrap().embedding_model, None);

        write_config(
            &dir,
            r#"{ "wiki_root": "~/W", "embedding_model": "BAAI/bge-small-zh-v1.5" }"#,
        );
        assert_eq!(
            AppConfig::load_from(&dir)
                .unwrap()
                .embedding_model
                .as_deref(),
            Some("BAAI/bge-small-zh-v1.5")
        );

        write_config(&dir, r#"{ "wiki_root": "~/W", "embedding_model": "" }"#);
        assert!(matches!(
            AppConfig::load_from(&dir),
            Err(AgentWikiError::Config(_))
        ));
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let (_t, dir) = tmp_dir();
        write_config(
            &dir,
            r#"{ "wiki_root": "~/W", "index_path": "/tmp/x", "extra": 42 }"#,
        );
        let cfg = AppConfig::load_from(&dir).expect("load");
        assert_eq!(cfg.wiki_root, home().join("W"));
    }

    #[test]
    fn existing_config_reports_its_file() {
        let (_t, dir) = tmp_dir();
        write_config(&dir, r#"{ "wiki_root": "~/W" }"#);
        let cfg = AppConfig::load_from(&dir).expect("load");
        assert_eq!(cfg.config_file, Some(dir.join(CONFIG_FILE_NAME)));
    }

    #[test]
    fn invalid_json_is_a_config_error() {
        let (_t, dir) = tmp_dir();
        write_config(&dir, "{ not json");
        assert!(matches!(
            AppConfig::load_from(&dir),
            Err(AgentWikiError::Config(_))
        ));
    }

    #[test]
    fn empty_wiki_root_is_rejected() {
        let (_t, dir) = tmp_dir();
        for bad in [r#"{ "wiki_root": "" }"#, r#"{ "wiki_root": "   " }"#] {
            write_config(&dir, bad);
            assert!(
                matches!(AppConfig::load_from(&dir), Err(AgentWikiError::Config(_))),
                "should reject {bad}"
            );
        }
    }
}
