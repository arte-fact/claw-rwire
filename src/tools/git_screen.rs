//! The bash git fence: agents inspect git freely but never write it — commits, branches,
//! merges, resets, and pushes belong to the harness (per pass, per restart, per Request).
//! This is a **net, not a wall** (agents aren't adversaries; they just type `git push`):
//! it screens the ordinary shapes a model produces, while `exec`'s hermetic git env
//! (no credential helpers, no prompts) backstops anything that slips through a subshell.

/// Read-only (plus fetch/clone) git subcommands agents may run through `bash`.
const SAFE_SUBCOMMANDS: [&str; 14] = [
    "status",
    "log",
    "diff",
    "show",
    "blame",
    "shortlog",
    "describe",
    "grep",
    "ls-files",
    "ls-tree",
    "rev-parse",
    "reflog",
    "fetch",
    "clone",
];

/// Flags on `fetch`/`clone` that execute commands or inject config (hooks, credential
/// helpers) — denied even though the subcommand itself is allowed.
const NETWORK_EXEC_FLAGS: [&str; 6] = [
    "--upload-pack",
    "-u",
    "--config",
    "-c",
    "--template",
    "--separate-git-dir",
];

/// Screen a bash command for git writes: `None` = run it, `Some(reason)` = refuse with the
/// reason. Splits on shell separators and checks every segment whose command is `git`.
pub fn screen(command: &str) -> Option<String> {
    for segment in command.split(['\n', ';', '|', '&', '(', ')', '`']) {
        if let Some(denied) = screen_segment(segment) {
            return Some(denied);
        }
    }
    None
}

fn screen_segment(segment: &str) -> Option<String> {
    let mut tokens = segment
        .split_whitespace()
        // `FOO=bar git …` and `env git …` still run git.
        .skip_while(|token| token.contains('=') && !token.starts_with('-') || *token == "env");
    let command = tokens.next()?;
    if command != "git" && !command.ends_with("/git") {
        return None;
    }
    // Global options before the subcommand: only `-C <dir>` (repo targeting) and pager
    // suppression pass — `-c`/`--git-dir`/… inject config, which is how hooks and
    // credential helpers come back.
    let mut subcommand = None;
    while let Some(token) = tokens.next() {
        match token {
            "-C" => {
                let _ = tokens.next();
            }
            "--no-pager" | "-P" => {}
            flag if flag.starts_with('-') => {
                return Some(deny(&format!("git global option {flag}")));
            }
            sub => {
                subcommand = Some(sub.to_owned());
                break;
            }
        }
    }
    let subcommand = subcommand?;
    if !SAFE_SUBCOMMANDS.contains(&subcommand.as_str()) {
        return Some(deny(&format!("git {subcommand}")));
    }
    if matches!(subcommand.as_str(), "fetch" | "clone") {
        for token in tokens {
            if NETWORK_EXEC_FLAGS
                .iter()
                .any(|flag| token == *flag || token.starts_with(&format!("{flag}=")))
            {
                return Some(deny(&format!("git {subcommand} {token}")));
            }
            // Transport-helper URLs (`ext::sh -c …`) execute commands.
            if token.contains("::") {
                return Some(deny(&format!(
                    "git {subcommand} with a {token:?} transport"
                )));
            }
        }
    }
    None
}

fn deny(what: &str) -> String {
    format!(
        "error: {what} is not allowed — git writes are managed by the harness: every \
         successful pass is committed automatically (with Task:/Run: trailers), a restart \
         resets to the pre-task state, and branches/merges/pushes belong to request \
         delivery. Inspect with repo_status / repo_log / repo_diff, or read-only git \
         (status, log, diff, show, blame, grep, ls-files, rev-parse, reflog, fetch, clone)."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_git_and_ordinary_commands_pass() {
        for command in [
            "git status",
            "git -C projects/app log --oneline -5",
            "git --no-pager diff HEAD~1",
            "cargo build && git diff --stat",
            "git clone https://github.com/foo/bar projects/bar",
            "grep -r 'git push' docs/",
            "cargo test",
            "GIT_PAGER=cat git show HEAD",
        ] {
            assert_eq!(screen(command), None, "should pass: {command}");
        }
    }

    #[test]
    fn git_writes_are_denied_wherever_they_hide() {
        for command in [
            "git push origin main",
            "git commit -am done",
            "git reset --hard HEAD~3",
            "git checkout -b feature",
            "git init",
            "git rebase -i HEAD~2",
            "cargo test && git commit -m 'works'",
            "echo ok; git push",
            "GIT_DIR=.git git push",
        ] {
            let denied = screen(command).expect(command);
            assert!(denied.contains("managed by the harness"), "{denied}");
        }
    }

    #[test]
    fn config_injection_and_exec_transports_are_denied() {
        for command in [
            "git -c core.hooksPath=/tmp/evil status",
            "git --git-dir=/tmp/x log",
            "git fetch --upload-pack=/tmp/evil origin",
            "git clone --template=/tmp/evil https://x/y",
            "git clone ext::sh-c-whoami repo",
            "git clone -c credential.helper=store https://x/y",
        ] {
            assert!(screen(command).is_some(), "should deny: {command}");
        }
    }
}
