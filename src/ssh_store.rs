//! Server-side SSH deploy-key store (P5b): private keys live as `0600` files under
//! `<data_dir>/ssh/<name>`, **never** in the DB — the registry keeps only the name + fingerprint.
//! The push path (`gitops`) reads a key by name to set `GIT_SSH_COMMAND -i <path>`; the admin UI
//! shows only the fingerprint, so the read-model can't leak the private material. The fingerprint is
//! the real SSH one (`ssh-keygen -l`), which also validates the key on write.

use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum SshStoreError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("blank or unsafe key name")]
    BadName,
    #[error("not a valid SSH private key")]
    Invalid,
}

/// The canonical, filename-safe form of a key name: ASCII alphanumerics plus `-` `_` `.`, with no
/// leading/trailing dots (so `.`/`..` can never form). The single source of truth for a key's
/// filename — store/read/remove all route names through it, so the DB name and the file agree.
#[must_use]
pub fn canonical_name(name: &str) -> Option<String> {
    let cleaned: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect();
    let trimmed = cleaned.trim_matches('.').to_owned();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn key_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("ssh")
}

/// The private-key file path for a canonical `name` — `None` if the name isn't filename-safe.
#[must_use]
pub fn key_path(data_dir: &Path, name: &str) -> Option<PathBuf> {
    canonical_name(name).map(|safe| key_dir(data_dir).join(safe))
}

/// Store a private key as a `0600` file under a `0700` dir and return its SSH fingerprint
/// (`SHA256:…`). A trailing newline is ensured (ssh is strict). The fingerprint doubles as
/// validation: an unparseable key is rejected and its file removed, so the store never holds an
/// unusable key.
pub fn store(data_dir: &Path, name: &str, private_key: &str) -> Result<String, SshStoreError> {
    let safe = canonical_name(name).ok_or(SshStoreError::BadName)?;
    let dir = key_dir(data_dir);
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    let path = dir.join(&safe);
    let body = if private_key.ends_with('\n') {
        private_key.to_owned()
    } else {
        format!("{private_key}\n")
    };
    write_private(&path, &body)?;
    match fingerprint(&path) {
        Ok(fp) => Ok(fp),
        Err(err) => {
            let _ = std::fs::remove_file(&path);
            Err(err)
        }
    }
}

/// Delete a named key's file. Idempotent — a missing file (or unsafe name) is not an error.
pub fn remove(data_dir: &Path, name: &str) -> Result<(), SshStoreError> {
    if let Some(path) = key_path(data_dir, name)
        && path.exists()
    {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

fn write_private(path: &Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(content.as_bytes())
}

/// The real SSH fingerprint of a key file via host `ssh-keygen -l -f` (stdin closed so an encrypted
/// key fails fast rather than prompting). Returns the `SHA256:…` field.
fn fingerprint(path: &Path) -> Result<String, SshStoreError> {
    let output = std::process::Command::new("ssh-keygen")
        .arg("-l")
        .arg("-f")
        .arg(path)
        .stdin(std::process::Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(SshStoreError::Invalid);
    }
    // `<bits> SHA256:<hash> <comment> (<type>)` — the fingerprint is the second field.
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .nth(1)
        .filter(|f| f.starts_with("SHA256:"))
        .map(str::to_owned)
        .ok_or(SshStoreError::Invalid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("claw-ssh-store-tests")
            .join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    /// A throwaway ed25519 keypair via host ssh-keygen — the store's own dependency, so tests that
    /// need a real key generate one the same way the operator would.
    fn generate_key(dir: &Path) -> String {
        let path = dir.join("gen");
        let ok = std::process::Command::new("ssh-keygen")
            .args(["-t", "ed25519", "-N", "", "-C", "test", "-f"])
            .arg(&path)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("ssh-keygen")
            .status
            .success();
        assert!(ok, "ssh-keygen must generate a key");
        std::fs::read_to_string(&path).expect("read generated key")
    }

    #[test]
    fn canonical_name_is_filename_safe_and_rejects_traversal() {
        assert_eq!(canonical_name("deploy.key"), Some("deploy.key".to_owned()));
        // Separators are stripped, so no name can ever escape the ssh dir; interior dots are kept
        // (a harmless single filename), and a name that is only dots reduces to nothing.
        assert_eq!(canonical_name("a/../b"), Some("a..b".to_owned()));
        assert!(!canonical_name("a/../b").unwrap().contains('/'));
        assert_eq!(canonical_name(".."), None);
        assert_eq!(canonical_name("."), None);
        assert_eq!(canonical_name("   "), None);
    }

    #[test]
    fn store_writes_a_0600_file_and_returns_a_sha256_fingerprint() {
        let data = scratch("store");
        let key = generate_key(&data);
        let fingerprint = store(&data, "deploy", &key).expect("store");
        assert!(fingerprint.starts_with("SHA256:"), "{fingerprint}");

        let path = key_path(&data, "deploy").expect("path");
        assert!(path.exists());
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "the private key is owner-read/write only"
        );
        assert_eq!(std::fs::read_to_string(&path).expect("read"), key);
    }

    #[test]
    fn store_rejects_a_non_key_and_leaves_nothing_behind() {
        let data = scratch("reject");
        assert!(matches!(
            store(&data, "bad", "not a private key at all\n"),
            Err(SshStoreError::Invalid)
        ));
        assert!(!key_path(&data, "bad").expect("path").exists());
    }

    #[test]
    fn remove_is_idempotent() {
        let data = scratch("remove");
        let key = generate_key(&data);
        store(&data, "deploy", &key).expect("store");
        remove(&data, "deploy").expect("remove");
        assert!(!key_path(&data, "deploy").expect("path").exists());
        remove(&data, "deploy").expect("second remove is a no-op");
    }
}
