//! Safe read-only command auto-approval — port of crush's
//! `internal/agent/tools/safe.go` + the safe-command detection in
//! `internal/agent/tools/bash.go:209-246`.
//!
//! A curated allowlist of read-only commands that bypass the permission
//! prompt entirely. Only single commands without chaining metacharacters
//! qualify. This eliminates permission fatigue for obviously-safe read
//! operations while maintaining safety for anything that modifies state.

/// Commands that are safe to auto-approve (read-only, no side effects).
/// Matched as a prefix: the command must start with one of these followed
/// by a space, hyphen, or end-of-string.
///
/// Wrapper/prefix commands (env, nice, nohup, set, time, timeout, unset)
/// are deliberately excluded: they approve whatever arbitrary inner
/// command follows them (e.g. `env rm -rf /`).
const SAFE_COMMANDS: &[&str] = &[
    "cal",
    "date",
    "df",
    "du",
    "echo",
    "free",
    "groups",
    "hostname",
    "id",
    "ls",
    "printenv",
    "ps",
    "pwd",
    "top",
    "type",
    "uname",
    "uptime",
    "whatis",
    "whereis",
    "which",
    "whoami",
];

/// Git subcommands that are read-only and safe to auto-approve.
/// Format: "git <subcommand>" — matched as prefix.
const SAFE_GIT_SUBCOMMANDS: &[&str] = &[
    "git blame",
    "git branch",
    "git config --get",
    "git config --list",
    "git describe",
    "git diff",
    "git grep",
    "git log",
    "git ls-files",
    "git ls-remote",
    "git remote",
    "git rev-parse",
    "git shortlog",
    "git show",
    "git status",
    "git tag",
];

/// Metacharacters that indicate command chaining or redirection — their
/// presence disqualifies auto-approval (the command could pipe into
/// something dangerous, or redirect output into a sensitive file).
const CHAINING_METACHARACTERS: &[&str] =
    &[";", "|", "&&", "$(", "`", ">", "<", ">>", "&", "\n"];

/// Per-subcommand denylist of flags that make an otherwise read-only git
/// subcommand mutate state (delete branches/tags, edit messages, write
/// files, run external diff tools, …). After a SAFE_GIT_SUBCOMMANDS
/// prefix matches, any token in the remainder that matches a denylisted
/// flag disqualifies auto-approval.
fn git_flag_denylist(subcommand: &str) -> Option<&'static [&'static str]> {
    match subcommand {
        "git branch" => Some(&["-d", "-D", "-m", "--edit", "-e", "--force", "-f"]),
        "git tag" => Some(&["-d", "-D", "-f", "-s", "-u", "--force", "--delete"]),
        "git diff" => Some(&["--output", "--ext-diff", "--no-index"]),
        _ => None,
    }
}

/// Check if a token matches a denylisted flag. Matches both the bare
/// flag (`--output`) and the `=`-attached value form (`--output=file`).
fn token_matches_flag(token: &str, flag: &str) -> bool {
    token == flag
        || token
            .strip_prefix(flag)
            .is_some_and(|rest| rest.starts_with('='))
}

/// Check if a command contains chaining metacharacters.
/// If so, it cannot be auto-approved regardless of the base command.
pub fn contains_command_chaining(command: &str) -> bool {
    CHAINING_METACHARACTERS.iter().any(|mc| command.contains(mc))
}

/// Check if a command is a safe read-only command that can bypass
/// the permission prompt.
///
/// Returns true if:
/// 1. The command does NOT contain chaining metacharacters
/// 2. The command starts with a known safe read-only command
/// 3. The character after the safe command prefix is a space, hyphen,
///    or end-of-string (prevents "lsof" matching "ls")
pub fn is_safe_read_only_command(command: &str) -> bool {
    if command.trim().is_empty() {
        return false;
    }
    // Chaining disqualifies.
    if contains_command_chaining(command) {
        return false;
    }
    let cmd_lower = command.trim().to_lowercase();

    // Check plain safe commands.
    for safe in SAFE_COMMANDS {
        if cmd_lower.starts_with(safe) {
            let next_char = cmd_lower.get(safe.len()..safe.len() + 1);
            match next_char {
                None => return true,              // exact match
                Some(" ") => return true,         // followed by argument
                Some("-") => return true,         // followed by flag
                _ => continue,                    // "lsof" doesn't match "ls"
            }
        }
    }

    // Check git read-only subcommands.
    for safe_git in SAFE_GIT_SUBCOMMANDS {
        if let Some(rest) = cmd_lower.strip_prefix(safe_git) {
            // Must be followed by space, hyphen, or end-of-string.
            if !(rest.is_empty() || rest.starts_with(' ') || rest.starts_with('-')) {
                continue;
            }
            // `git remote` is only safe in its bare listing forms: any
            // mutating subcommand (add/remove/rename/set-url/…) must be
            // rejected. Safe usage: `git remote`, `git remote -v`,
            // `git remote show <name>`.
            if *safe_git == "git remote" {
                let tokens: Vec<&str> = rest.split_whitespace().collect();
                let is_bare_or_verbose = tokens.is_empty()
                    || tokens == ["-v"]
                    || tokens == ["--verbose"]
                    || (tokens.first() == Some(&"show") && tokens.len() == 2);
                if !is_bare_or_verbose {
                    return false;
                }
            }
            // Reject denylisted destructive flags for this subcommand.
            if let Some(denied) = git_flag_denylist(safe_git) {
                let has_denied_flag = rest
                    .split_whitespace()
                    .any(|token| denied.iter().any(|flag| token_matches_flag(token, flag)));
                if has_denied_flag {
                    return false;
                }
            }
            return true;
        }
    }

    false
}

/// Check if a command is a write/mutation command that should require
/// explicit permission. This is the inverse check — even if chaining
/// is absent, some commands are inherently dangerous.
pub fn is_dangerous_command(command: &str) -> bool {
    let cmd_lower = command.trim().to_lowercase();
    let dangerous = [
        "rm ",
        "rmdir",
        "sudo",
        "chmod",
        "chown",
        "kill ",
        "killall",
        "pkill",
        "reboot",
        "shutdown",
        "halt",
        "mkfs",
        "dd ",
        "git push",
        "git commit",
        "git merge",
        "git rebase",
        "git reset",
        "git checkout",
        "git clean",
        "npm install",
        "npm uninstall",
        "pip install",
        "pip uninstall",
        "cargo install",
        "brew install",
        "brew uninstall",
        "apt install",
        "apt-get install",
        "yum install",
        "dnf install",
        "pacman -S",
    ];
    for d in &dangerous {
        if cmd_lower.starts_with(d) {
            return true;
        }
    }
    false
}

// ─── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safe_simple_commands() {
        assert!(is_safe_read_only_command("ls"));
        assert!(is_safe_read_only_command("ls -la"));
        assert!(is_safe_read_only_command("ls /tmp"));
        assert!(is_safe_read_only_command("pwd"));
        assert!(is_safe_read_only_command("echo hello"));
        assert!(is_safe_read_only_command("date"));
        assert!(is_safe_read_only_command("which python"));
        assert!(is_safe_read_only_command("ps aux"));
    }

    #[test]
    fn test_safe_git_commands() {
        assert!(is_safe_read_only_command("git status"));
        assert!(is_safe_read_only_command("git log --oneline"));
        assert!(is_safe_read_only_command("git diff"));
        assert!(is_safe_read_only_command("git branch -a"));
        assert!(is_safe_read_only_command("git show HEAD"));
    }

    #[test]
    fn test_unsafe_commands() {
        assert!(!is_safe_read_only_command("rm -rf /"));
        assert!(!is_safe_read_only_command("git push"));
        assert!(!is_safe_read_only_command("git commit -m test"));
        assert!(!is_safe_read_only_command("npm install"));
        assert!(!is_safe_read_only_command("sudo ls"));
        assert!(!is_safe_read_only_command("cargo build"));
    }

    #[test]
    fn test_prefix_not_matching() {
        // "lsof" should NOT match "ls" prefix.
        assert!(!is_safe_read_only_command("lsof"));
        // "gps" should NOT match "ps".
        assert!(!is_safe_read_only_command("gps"));
        // "_ls" should NOT match "ls".
        assert!(!is_safe_read_only_command("_ls"));
    }

    #[test]
    fn test_chaining_disqualifies() {
        assert!(!is_safe_read_only_command("ls | grep foo"));
        assert!(!is_safe_read_only_command("ls && rm file"));
        assert!(!is_safe_read_only_command("ls; cat file"));
        assert!(!is_safe_read_only_command("echo $(whoami)"));
        assert!(!is_safe_read_only_command("ls `cat file`"));
    }

    #[test]
    fn test_dangerous_commands() {
        assert!(is_dangerous_command("rm -rf /tmp"));
        assert!(is_dangerous_command("sudo apt install foo"));
        assert!(is_dangerous_command("git push origin main"));
        assert!(is_dangerous_command("npm install express"));
        assert!(!is_dangerous_command("ls -la"));
        assert!(!is_dangerous_command("git status"));
    }

    #[test]
    fn test_empty_command() {
        assert!(!is_safe_read_only_command(""));
        assert!(!is_safe_read_only_command("   "));
    }

    #[test]
    fn test_case_insensitive() {
        assert!(is_safe_read_only_command("LS"));
        assert!(is_safe_read_only_command("Git Status"));
    }

    #[test]
    fn test_wrapper_prefixes_not_safe() {
        // Finding #1: wrapper prefixes must not auto-approve inner commands.
        assert!(!is_safe_read_only_command("env rm -rf /"));
        assert!(!is_safe_read_only_command("nice rm -rf /"));
        assert!(!is_safe_read_only_command("nohup rm -rf /"));
        assert!(!is_safe_read_only_command("timeout 10 rm -rf /"));
        assert!(!is_safe_read_only_command("time rm -rf /"));
        assert!(!is_safe_read_only_command("set VAR=1"));
        assert!(!is_safe_read_only_command("unset PATH"));
        assert!(!is_safe_read_only_command("env"));
    }

    #[test]
    fn test_redirection_not_safe() {
        // Finding #2: redirection must not bypass approval.
        assert!(!is_safe_read_only_command("echo x > file"));
        assert!(!is_safe_read_only_command("echo pwned > ~/.ssh/authorized_keys"));
        assert!(!is_safe_read_only_command("cat < file"));
        assert!(!is_safe_read_only_command("echo x >> file"));
        assert!(!is_safe_read_only_command("ls & bg"));
        assert!(!is_safe_read_only_command("ls\nrm -rf /"));
    }

    #[test]
    fn test_git_destructive_flags_not_safe() {
        // Finding #3: destructive git flag suffixes must be rejected.
        assert!(!is_safe_read_only_command("git branch -d main"));
        assert!(!is_safe_read_only_command("git branch -D main"));
        assert!(!is_safe_read_only_command("git branch -m new-name"));
        assert!(!is_safe_read_only_command("git tag -f v1"));
        assert!(!is_safe_read_only_command("git tag -d v1"));
        assert!(!is_safe_read_only_command("git tag --delete v1"));
        assert!(!is_safe_read_only_command("git diff --output=/tmp/x HEAD~"));
        assert!(!is_safe_read_only_command("git diff --output f HEAD~1"));
        assert!(!is_safe_read_only_command("git diff --ext-diff HEAD~1"));
        assert!(!is_safe_read_only_command("git diff --no-index a b"));
        assert!(!is_safe_read_only_command("git remote add x https://y"));
        assert!(!is_safe_read_only_command("git remote remove origin"));
        assert!(!is_safe_read_only_command("git remote rename a b"));
        assert!(!is_safe_read_only_command("git remote set-url origin https://y"));
    }

    #[test]
    fn test_still_safe_after_hardening() {
        // Previously-safe read-only usage must remain auto-approved.
        assert!(is_safe_read_only_command("ls -la"));
        assert!(is_safe_read_only_command("git status"));
        assert!(is_safe_read_only_command("git diff HEAD~1"));
        assert!(is_safe_read_only_command("git branch -a"));
        assert!(is_safe_read_only_command("git remote"));
        assert!(is_safe_read_only_command("git remote -v"));
        assert!(is_safe_read_only_command("git remote show origin"));
        assert!(is_safe_read_only_command("git config --get user.name"));
        assert!(is_safe_read_only_command("git config --list"));
    }
}
