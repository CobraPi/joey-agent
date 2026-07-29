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
const SAFE_COMMANDS: &[&str] = &[
    "cal",
    "date",
    "df",
    "du",
    "echo",
    "env",
    "free",
    "groups",
    "hostname",
    "id",
    "ls",
    "nice",
    "nohup",
    "printenv",
    "ps",
    "pwd",
    "set",
    "time",
    "timeout",
    "top",
    "type",
    "uname",
    "unset",
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

/// Metacharacters that indicate command chaining — their presence
/// disqualifies auto-approval (the command could pipe into something
/// dangerous).
const CHAINING_METACHARACTERS: &[&str] = &[";", "|", "&&", "$(", "`"];

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
            if rest.is_empty() || rest.starts_with(' ') || rest.starts_with('-') {
                return true;
            }
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
}
