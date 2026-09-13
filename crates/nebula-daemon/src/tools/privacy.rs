use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

pub fn permitted(path: &Path, roots: &[PathBuf]) -> Result<PathBuf> {
    let resolved = path
        .canonicalize()
        .map_err(|_| anyhow::anyhow!("path is unavailable"))?;
    if !roots
        .iter()
        .filter_map(|root| root.canonicalize().ok())
        .any(|root| resolved.starts_with(root))
        || is_protected(&resolved)
    {
        bail!("path is outside configured folders or belongs to a protected credential location");
    }
    Ok(resolved)
}

pub fn is_protected(path: &Path) -> bool {
    if ["/proc", "/sys", "/dev", "/etc", "/run"]
        .iter()
        .any(|root| path.starts_with(root))
    {
        return true;
    }
    path.components().any(|part| {
        let name = part.as_os_str().to_string_lossy().to_lowercase();
        [
            ".ssh",
            ".gnupg",
            ".aws",
            ".azure",
            ".kube",
            ".docker",
            "keyrings",
            "gcloud",
            "nebula.env",
            "shadow",
            ".netrc",
            ".npmrc",
            ".pypirc",
            "auth.json",
            "codex-accounts",
            ".password-store",
        ]
        .contains(&name.as_str())
            || name.starts_with(".env")
            || name.starts_with("id_rsa")
            || name.starts_with("id_ed25519")
            || name.contains("credential")
            || name.contains("token")
            || name.contains("secret")
            || name.ends_with(".pem")
            || name.ends_with(".key")
            || name.ends_with(".p12")
            || name == "cookies"
            || name == "login data"
            || name == "logins.json"
            || name == "key4.db"
    })
}

pub fn redact(text: &str) -> String {
    let mut private_key = false;
    text.split_inclusive('\n')
        .map(|line| {
            let upper = line.to_ascii_uppercase();
            if upper.contains("-----BEGIN") && upper.contains("PRIVATE KEY-----") {
                private_key = true;
            }
            let hidden = private_key
                || [
                    "PASSWORD",
                    "PASSWD",
                    "API_KEY",
                    "API-KEY",
                    "SECRET",
                    "ACCESS_TOKEN",
                    "REFRESH_TOKEN",
                    "AUTHORIZATION:",
                    "BEARER ",
                    "COOKIE:",
                    "SET-COOKIE:",
                    "SK-PROJ-",
                    "SK-SVCACCT-",
                    "SSH-ED25519",
                    "SSH-RSA",
                ]
                .iter()
                .any(|needle| upper.contains(needle));
            if upper.contains("-----END") && upper.contains("PRIVATE KEY-----") {
                private_key = false;
            }
            if hidden {
                if line.ends_with("\r\n") {
                    "[credential content withheld]\r\n"
                } else if line.ends_with('\n') {
                    "[credential content withheld]\n"
                } else {
                    "[credential content withheld]"
                }
            } else {
                line
            }
        })
        .collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keys_and_assignments_are_withheld() {
        let result = redact(
            "hello\nPASSWORD=private-value\n-----BEGIN OPENSSH PRIVATE KEY-----\nopaque-data\n-----END OPENSSH PRIVATE KEY-----\nworld",
        );
        assert!(!result.contains("private-value"));
        assert!(!result.contains("opaque-data"));
        assert!(result.contains("world"));
    }
    #[test]
    fn credential_paths_and_symlink_escapes_are_denied() {
        let root = std::env::temp_dir().join(format!("nebula-path-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let secret = root.join(".env");
        std::fs::write(&secret, "not-a-real-secret").unwrap();
        let link = root.join("innocent.txt");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&secret, &link).unwrap();
        assert!(permitted(&link, std::slice::from_ref(&root)).is_err());
        assert!(permitted(Path::new("/etc/passwd"), std::slice::from_ref(&root)).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
