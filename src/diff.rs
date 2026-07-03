//! A minimal line-level diff (LCS-based) for showing what an `edit` tool call changed — its
//! `old_string` vs `new_string`, straight from the run transcript (no before-snapshot needed).
//! Inputs are search/replace snippets, so the O(n·m) DP is fine.

/// Whether a diff line is unchanged, added, or removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    Equal,
    Added,
    Removed,
}

/// One line of a diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffKind,
    pub text: String,
}

/// Line diff of `old` → `new`, aligned on the longest common subsequence of lines.
pub fn line_diff(old: &str, new: &str) -> Vec<DiffLine> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let (n, m) = (a.len(), b.len());
    // dp[i][j] = LCS length of a[i..] and b[j..].
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut out = Vec::new();
    let line = |kind, text: &str| DiffLine {
        kind,
        text: text.to_owned(),
    };
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            out.push(line(DiffKind::Equal, a[i]));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            out.push(line(DiffKind::Removed, a[i]));
            i += 1;
        } else {
            out.push(line(DiffKind::Added, b[j]));
            j += 1;
        }
    }
    while i < n {
        out.push(line(DiffKind::Removed, a[i]));
        i += 1;
    }
    while j < m {
        out.push(line(DiffKind::Added, b[j]));
        j += 1;
    }
    out
}

/// Classify a unified git patch's lines for rendering with the same vocabulary: `+` lines
/// are Added, `-` Removed, and everything else (context, hunk/file headers) Equal — the
/// `+++`/`---` file headers stay context, not churn.
pub fn patch_lines(patch: &str) -> Vec<DiffLine> {
    patch
        .lines()
        .map(|line| {
            let kind = if line.starts_with("+++") || line.starts_with("---") {
                DiffKind::Equal
            } else if line.starts_with('+') {
                DiffKind::Added
            } else if line.starts_with('-') {
                DiffKind::Removed
            } else {
                DiffKind::Equal
            };
            DiffLine {
                kind,
                text: line.to_owned(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(d: &[DiffLine]) -> Vec<(DiffKind, &str)> {
        d.iter().map(|l| (l.kind, l.text.as_str())).collect()
    }

    #[test]
    fn identical_is_all_equal() {
        let d = line_diff("a\nb", "a\nb");
        assert_eq!(
            kinds(&d),
            vec![(DiffKind::Equal, "a"), (DiffKind::Equal, "b")]
        );
    }

    #[test]
    fn one_line_changed_shows_removed_then_added_in_context() {
        let d = line_diff("a\nb\nc", "a\nB\nc");
        assert_eq!(
            kinds(&d),
            vec![
                (DiffKind::Equal, "a"),
                (DiffKind::Removed, "b"),
                (DiffKind::Added, "B"),
                (DiffKind::Equal, "c"),
            ]
        );
    }

    #[test]
    fn pure_addition_and_removal() {
        assert_eq!(
            kinds(&line_diff("a", "a\nb")),
            vec![(DiffKind::Equal, "a"), (DiffKind::Added, "b")]
        );
        assert_eq!(
            kinds(&line_diff("a\nb", "a")),
            vec![(DiffKind::Equal, "a"), (DiffKind::Removed, "b")]
        );
    }

    #[test]
    fn patch_lines_classifies_a_unified_git_patch() {
        let patch = "diff --git a/x.md b/x.md\n--- a/x.md\n+++ b/x.md\n@@ -1,2 +1,2 @@\n context\n-old\n+new";
        let kinds: Vec<DiffKind> = patch_lines(patch).iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            vec![
                DiffKind::Equal, // diff --git
                DiffKind::Equal, // --- header, not a removal
                DiffKind::Equal, // +++ header, not an addition
                DiffKind::Equal, // @@ hunk
                DiffKind::Equal, // context
                DiffKind::Removed,
                DiffKind::Added,
            ]
        );
    }

    #[test]
    fn reassembles_old_and_new() {
        // Removed+Equal lines reassemble `old`; Added+Equal reassemble `new`.
        let (old, new) = ("one\ntwo\nthree", "one\nthree\nfour");
        let d = line_diff(old, new);
        let from_old: Vec<&str> = d
            .iter()
            .filter(|l| l.kind != DiffKind::Added)
            .map(|l| l.text.as_str())
            .collect();
        let from_new: Vec<&str> = d
            .iter()
            .filter(|l| l.kind != DiffKind::Removed)
            .map(|l| l.text.as_str())
            .collect();
        assert_eq!(from_old.join("\n"), old);
        assert_eq!(from_new.join("\n"), new);
    }
}
