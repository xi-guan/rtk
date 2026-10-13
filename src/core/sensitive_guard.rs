//! blocks read access to sensitive files (keys, secrets, credentials).

use anyhow::{Result, bail};
use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;

static SENSITIVE_RE: OnceLock<Regex> = OnceLock::new();

fn sensitive_regex() -> &'static Regex {
    SENSITIVE_RE.get_or_init(|| {
        Regex::new(concat!(
            r"(?i)(^|/)(\.env($|[./])|\.envrc(/|$)|\.ssh(/|$)|\.aws(/|$)",
            r"|\.gcloud(/|$)|\.azure(/|$)|\.docker/config\.json|\.kube/config",
            r"|private_key(/|$)|secret(/|$)|credential(/|$)|token\.json$)",
        ))
        .expect("invalid guard regex")
    })
}

const SENSITIVE_EXTS: &[&str] = &["key", "pem", "p12", "pfx", "crt", "ppk", "gpg"];

pub fn check_path(path: &Path) -> Result<()> {
    if let Some(ext) = path.extension().and_then(|e| e.to_str())
        && SENSITIVE_EXTS.contains(&ext)
    {
        bail!("rtk: blocked sensitive file (.{ext}): {}", path.display());
    }

    let s = path.to_string_lossy();
    if sensitive_regex().is_match(&s) {
        bail!("rtk: blocked sensitive path: {}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn blocks_dotenv() {
        assert!(check_path(Path::new("/app/.env")).is_err());
        assert!(check_path(Path::new("/app/.env.local")).is_err());
        assert!(check_path(Path::new("/app/.envrc")).is_err());
    }

    #[test]
    fn blocks_key_extensions() {
        assert!(check_path(Path::new("/home/user/server.pem")).is_err());
        assert!(check_path(Path::new("/tmp/id_rsa.key")).is_err());
        assert!(check_path(Path::new("cert.p12")).is_err());
        assert!(check_path(Path::new("deploy.ppk")).is_err());
    }

    #[test]
    fn blocks_cloud_config_dirs() {
        assert!(check_path(Path::new("/home/user/.ssh/id_rsa")).is_err());
        assert!(check_path(Path::new("/home/user/.aws/credentials")).is_err());
        assert!(check_path(Path::new("/home/user/.kube/config")).is_err());
        assert!(check_path(Path::new("/home/user/.docker/config.json")).is_err());
        assert!(check_path(Path::new("/home/user/.gcloud/key.json")).is_err());
        assert!(check_path(Path::new("/home/user/.azure/token")).is_err());
    }

    #[test]
    fn blocks_secret_directory() {
        assert!(check_path(Path::new("/app/secret/db.yaml")).is_err());
        assert!(check_path(Path::new("/app/credential/aws.json")).is_err());
        assert!(check_path(Path::new("/app/private_key/rsa")).is_err());
    }

    #[test]
    fn allows_normal_files() {
        assert!(check_path(Path::new("src/main.rs")).is_ok());
        assert!(check_path(Path::new("README.md")).is_ok());
        assert!(check_path(Path::new("Cargo.toml")).is_ok());
        assert!(check_path(Path::new("/app/config/settings.json")).is_ok());
    }

    #[test]
    fn allows_secret_as_substring() {
        assert!(check_path(Path::new("src/secret_manager.rs")).is_ok());
        assert!(check_path(Path::new("lib/credential_helper.py")).is_ok());
        assert!(check_path(Path::new("pkg/private_key_utils.go")).is_ok());
    }

    #[test]
    fn blocks_relative_paths() {
        assert!(check_path(Path::new(".env")).is_err());
        assert!(check_path(Path::new(".env.local")).is_err());
        assert!(check_path(Path::new(".envrc")).is_err());
        assert!(check_path(Path::new(".ssh/id_rsa")).is_err());
        assert!(check_path(Path::new(".aws/credentials")).is_err());
        assert!(check_path(Path::new(".kube/config")).is_err());
        assert!(check_path(Path::new(".docker/config.json")).is_err());
        assert!(check_path(Path::new("secret/db.yaml")).is_err());
        assert!(check_path(Path::new("credential/aws.json")).is_err());
        assert!(check_path(Path::new("private_key/rsa")).is_err());
        assert!(check_path(Path::new("token.json")).is_err());
    }

    #[test]
    fn blocks_token_json_at_end() {
        assert!(check_path(Path::new("/app/token.json")).is_err());
        assert!(check_path(Path::new("/app/token.json.bak")).is_ok());
    }
}
