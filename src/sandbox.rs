//! Sandbox mode for agent command execution (R6). `Host` runs `bash` directly in the workspace
//! dir (today's behaviour); `Isola` runs it inside the workspace's rootless [isola] sandbox so a
//! stray command can't touch the host beyond the bind-mounted workspace.
//!
//! Resolved from the `CLAW_SANDBOX` env (via the bridge's env helper). [`SandboxMode::resolve`] is
//! a pure string → mode map; whether isola is actually compiled in for this build/platform is the
//! separate [`SandboxMode::isola_available`] check, so the bridge can fall back to `Host` (and log
//! once) when `isola` is requested on a host/build that lacks it.
//!
//! [isola]: https://github.com/ (first-party `../isola`)

/// Where an agent's `bash` commands run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SandboxMode {
    /// Spawn `bash` on the host, in the workspace dir. The default and pre-R6 behaviour.
    #[default]
    Host,
    /// Run `bash` inside the workspace's isola sandbox.
    Isola,
}

impl SandboxMode {
    /// Map a requested mode string (`CLAW_SANDBOX`) to a [`SandboxMode`]. `"isola"` (any casing,
    /// trimmed) selects [`SandboxMode::Isola`]; everything else — including `None` — is `Host`.
    #[must_use]
    pub fn resolve(requested: Option<&str>) -> Self {
        match requested.map(|value| value.trim().to_ascii_lowercase()) {
            Some(value) if value == "isola" => Self::Isola,
            _ => Self::Host,
        }
    }

    /// Whether isola-backed execution is compiled in for this build and platform. When this is
    /// false, a resolved [`SandboxMode::Isola`] must degrade to `Host` (the engine can't sandbox).
    #[must_use]
    pub const fn isola_available() -> bool {
        cfg!(all(feature = "sandbox", target_os = "linux"))
    }

    /// The effective mode: the resolved mode, demoted to `Host` when isola isn't available.
    #[must_use]
    pub fn effective(self) -> Self {
        match self {
            Self::Isola if !Self::isola_available() => Self::Host,
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_host() {
        assert_eq!(SandboxMode::resolve(None), SandboxMode::Host);
        assert_eq!(SandboxMode::default(), SandboxMode::Host);
    }

    #[test]
    fn reads_isola_from_env() {
        assert_eq!(SandboxMode::resolve(Some("isola")), SandboxMode::Isola);
        assert_eq!(SandboxMode::resolve(Some("  ISOLA ")), SandboxMode::Isola);
    }

    #[test]
    fn unknown_value_is_host() {
        assert_eq!(SandboxMode::resolve(Some("host")), SandboxMode::Host);
        assert_eq!(SandboxMode::resolve(Some("")), SandboxMode::Host);
        assert_eq!(SandboxMode::resolve(Some("docker")), SandboxMode::Host);
    }

    #[test]
    fn effective_demotes_isola_when_unavailable() {
        // With the `sandbox` feature off, isola isn't available → Isola demotes to Host.
        let effective = SandboxMode::Isola.effective();
        if SandboxMode::isola_available() {
            assert_eq!(effective, SandboxMode::Isola);
        } else {
            assert_eq!(effective, SandboxMode::Host);
        }
        assert_eq!(SandboxMode::Host.effective(), SandboxMode::Host);
    }
}
