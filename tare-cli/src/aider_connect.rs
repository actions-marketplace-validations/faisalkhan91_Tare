//! Additive, reversible auto-wiring of Aider's `.env` to route through Tare's capture proxy.
//! Aider reads `AIDER_OPENAI_API_BASE` from a `.env` file (searched: home, git root,
//! cwd, `--env-file`; later files win — verified against aider.chat/docs/config/dotenv).
//!
//! We MANAGE a delimited Tare block appended to the chosen `.env`:
//!   - **Parser-free** — a `.env` is line-based `KEY=value`, so no YAML/JSON dependency.
//!   - **Format-preserving** — everything OUTSIDE the `# >>> tare >>>` … `# <<< tare <<<` block is
//!     left byte-for-byte; a user's own keys/comments are never touched.
//!   - **Reversible** — `disconnect` removes exactly the block (`connect` then `disconnect` restores
//!     the original, modulo a normalized trailing newline).
//!   - **Scoped** — the CLI defaults to the PROJECT `./.env`, never the shared `~/.env`, so we never
//!     silently redirect other dotenv-using tools. Opt-in; nothing runs on its own.

const BEGIN: &str = "# >>> tare (managed) >>>";
const END: &str = "# <<< tare <<<";

/// Remove the Tare-managed block (`BEGIN`..=`END` inclusive) from `.env` text, leaving every other
/// line intact. Idempotent (no block → returns the content normalized to a single trailing newline).
fn strip_block(env: &str) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut in_block = false;
    for line in env.lines() {
        let t = line.trim();
        if t == BEGIN {
            in_block = true;
            continue;
        }
        if in_block {
            if t == END {
                in_block = false;
            }
            continue;
        }
        kept.push(line);
    }
    // Drop trailing blank lines (e.g. the separator the block left behind), then end with exactly one
    // newline. Only whole blank lines are removed — a value line's own trailing spaces are preserved.
    while kept.last().is_some_and(|l| l.trim().is_empty()) {
        kept.pop();
    }
    let mut s = kept.join("\n");
    if !s.is_empty() {
        s.push('\n');
    }
    s
}

/// `.env` text with a fresh Tare block wiring `AIDER_OPENAI_API_BASE` to `base_url`. Any prior Tare
/// block is replaced (re-connect is idempotent). Content outside the block is preserved.
pub fn aider_env_connect(env: &str, base_url: &str) -> String {
    let mut s = strip_block(env);
    if !s.is_empty() {
        s.push('\n'); // a blank line separating the user's content from our block
    }
    s.push_str(BEGIN);
    s.push('\n');
    s.push_str("# Managed by `tare aider-connect` — routes Aider through Tare's capture proxy.\n");
    s.push_str("# Reverse with `tare aider-disconnect` (or delete this block).\n");
    s.push_str(&format!("AIDER_OPENAI_API_BASE={base_url}\n"));
    s.push_str(END);
    s.push('\n');
    s
}

/// `.env` text with the Tare block removed — reverses [`aider_env_connect`] exactly. Idempotent.
pub fn aider_env_disconnect(env: &str) -> String {
    strip_block(env)
}

/// Default target: the PROJECT-local `./.env` (never the shared home `.env`). Aider loads it from cwd.
pub fn default_aider_env() -> String {
    "./.env".to_string()
}

/// Default proxy base for Aider (an OpenAI-compatible client → the `/v1` base, like `connectEnvFor`).
pub fn default_aider_base() -> String {
    "http://127.0.0.1:8788/v1".to_string()
}

/// Wire Aider by managing the Tare block in `path` (created if absent). Reversible via disconnect.
pub fn aider_connect_command(path: &str, base_url: &str) -> Result<(), String> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let updated = aider_env_connect(&existing, base_url);
    std::fs::write(path, &updated).map_err(|e| format!("write {path}: {e}"))?;
    println!(
        "tare: wired Aider → {base_url} in {path}\n      (additive + reversible — undo with `tare aider-disconnect`)"
    );
    Ok(())
}

/// Remove Tare's Aider wiring from `path`. A missing file / absent block is a no-op, not an error.
pub fn aider_disconnect_command(path: &str) -> Result<(), String> {
    let Ok(existing) = std::fs::read_to_string(path) else {
        println!("tare: no {path} to disconnect");
        return Ok(());
    };
    let updated = aider_env_disconnect(&existing);
    std::fs::write(path, &updated).map_err(|e| format!("write {path}: {e}"))?;
    println!("tare: removed Tare's Aider wiring from {path}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_adds_a_reversible_block_and_preserves_user_content() {
        let user = "OPENAI_API_KEY=sk-secret\nFOO=bar\n";
        let wired = aider_env_connect(user, "http://127.0.0.1:8788/v1");
        // Our block is present with the right key…
        assert!(wired.contains(BEGIN) && wired.contains(END));
        assert!(wired.contains("AIDER_OPENAI_API_BASE=http://127.0.0.1:8788/v1"));
        // …and the user's own lines are untouched.
        assert!(wired.contains("OPENAI_API_KEY=sk-secret"));
        assert!(wired.contains("FOO=bar"));
        // Disconnect restores the original exactly (byte-for-byte).
        assert_eq!(aider_env_disconnect(&wired), user);
    }

    #[test]
    fn reconnect_is_idempotent_single_block() {
        let once = aider_env_connect("A=1\n", "http://h:1/v1");
        let twice = aider_env_connect(&once, "http://h:2/v1");
        // Exactly one managed block, and the latest base_url wins.
        assert_eq!(twice.matches(BEGIN).count(), 1);
        assert_eq!(twice.matches(END).count(), 1);
        assert!(twice.contains("AIDER_OPENAI_API_BASE=http://h:2/v1"));
        assert!(!twice.contains("http://h:1/v1"));
        // The user's line still survives the re-wire.
        assert!(twice.contains("A=1"));
    }

    #[test]
    fn connect_on_empty_env_and_disconnect_round_trip() {
        let wired = aider_env_connect("", "http://h:1/v1");
        assert!(wired.starts_with(BEGIN)); // no leading blank line when the file was empty
        assert!(wired.contains("AIDER_OPENAI_API_BASE=http://h:1/v1"));
        assert_eq!(aider_env_disconnect(&wired), ""); // fully removed
    }

    #[test]
    fn disconnect_on_content_without_block_leaves_it_intact() {
        assert_eq!(aider_env_disconnect("X=1\nY=2\n"), "X=1\nY=2\n");
    }
}
