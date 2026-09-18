//! Git-native cost attribution: the PURE parser that turns raw `git` command output
//! into a payload-free [`GitAttribution`] (SHA / branch / author / dirty). The CLI runs the git
//! commands at the edge (I/O) and hands their stdout here; the core never shells out and never reads
//! a clock. This is the identity a run is stamped with so spend can later roll up by commit/author
//! ("git blame, for cost") — distinct from the per-task denominator.
//!
//! Counts-only: only the commit SHA, branch name, and author identity are retained — never diffs,
//! messages, or file contents.

use serde::{Deserialize, Serialize};

/// The git identity of the working tree when a run executed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitAttribution {
    /// Full commit SHA (`git rev-parse HEAD`).
    pub sha: String,
    /// First 12 chars of the SHA — the human-facing short form.
    pub short_sha: String,
    /// Branch name, or `None` on a detached HEAD.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Commit author (`git log -1 --format=%an`), or `None` if unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// True if the working tree had uncommitted changes (`git status --porcelain` non-empty) — so
    /// spend attributed to this SHA is flagged as not-exactly-that-commit.
    pub dirty: bool,
}

/// Trim + cap a git field (SHA/branch/author are short; guard against a hostile/huge value).
fn field(s: &str, cap: usize) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.chars().take(cap).collect())
    }
}

/// Parse the raw stdout of the git commands into a [`GitAttribution`]:
/// - `rev_parse_head`: `git rev-parse HEAD`
/// - `branch`: `git symbolic-ref --quiet --short HEAD` (EMPTY on detached HEAD)
/// - `author`: `git log -1 --format=%an`
/// - `status_porcelain`: `git status --porcelain` (non-empty ⇒ dirty)
///
/// Returns `None` when there's no resolvable HEAD (not a repo / empty output) — the caller then
/// simply doesn't stamp git attribution.
pub fn parse_git_attribution(
    rev_parse_head: &str,
    branch: &str,
    author: &str,
    status_porcelain: &str,
) -> Option<GitAttribution> {
    let sha = field(rev_parse_head, 64)?;
    // A valid SHA is hex; reject an error string that slipped through (e.g. "fatal: ...").
    if !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(GitAttribution {
        short_sha: sha.chars().take(12).collect(),
        sha,
        branch: field(branch, 128),
        author: field(author, 128),
        dirty: !status_porcelain.trim().is_empty(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_clean_branch_checkout() {
        let g = parse_git_attribution(
            "9ee39e6abc1234567890deadbeef\n",
            "build/v0.1\n",
            "Ada Lovelace\n",
            "",
        )
        .unwrap();
        assert_eq!(g.sha, "9ee39e6abc1234567890deadbeef");
        assert_eq!(g.short_sha, "9ee39e6abc12");
        assert_eq!(g.branch.as_deref(), Some("build/v0.1"));
        assert_eq!(g.author.as_deref(), Some("Ada Lovelace"));
        assert!(!g.dirty);
    }

    #[test]
    fn detached_head_has_no_branch() {
        let g = parse_git_attribution("abc123def456", "", "Dev", "").unwrap();
        assert_eq!(g.branch, None, "empty symbolic-ref ⇒ detached HEAD");
        assert_eq!(g.short_sha, "abc123def456");
    }

    #[test]
    fn dirty_tree_is_flagged() {
        let g =
            parse_git_attribution("abc123", "main", "Dev", " M src/lib.rs\n?? new.txt\n").unwrap();
        assert!(g.dirty, "uncommitted changes ⇒ dirty");
    }

    #[test]
    fn rejects_non_repo_and_error_output() {
        // Empty rev-parse (not a repo) → None.
        assert!(parse_git_attribution("", "", "", "").is_none());
        // A git error string that isn't a hex SHA → None (never mistaken for a commit).
        assert!(parse_git_attribution("fatal: not a git repository", "", "", "").is_none());
    }
}
