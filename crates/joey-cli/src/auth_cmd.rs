//! `joey auth copilot` — GitHub Copilot credential management.

use std::time::Duration;

use anyhow::Result;
use clap::{Args, Subcommand};
use joey_core::config::save_env_value;

#[derive(Args, Debug)]
pub struct AuthArgs {
    #[command(subcommand)]
    command: AuthCommand,
}

#[derive(Subcommand, Debug)]
enum AuthCommand {
    /// Manage GitHub Copilot OAuth/token authentication.
    Copilot(CopilotArgs),
}

#[derive(Args, Debug)]
struct CopilotArgs {
    #[command(subcommand)]
    command: CopilotCommand,
}

#[derive(Subcommand, Debug)]
enum CopilotCommand {
    /// Authenticate with GitHub's OAuth device-code flow.
    Login,
    /// Show the active Copilot credential source.
    Status,
    /// Remove Copilot tokens stored in Joey's .env file.
    Logout,
}

pub fn auth_command(args: AuthArgs) -> Result<i32> {
    match args.command {
        AuthCommand::Copilot(args) => match args.command {
            CopilotCommand::Login => {
                let token = joey_providers::copilot::device_code_login(Duration::from_secs(300))?;
                save_env_value("COPILOT_GITHUB_TOKEN", &token)?;
                std::env::set_var("COPILOT_GITHUB_TOKEN", &token);
                println!("Logged in to GitHub Copilot. Token saved to ~/.joey/.env.");
                Ok(0)
            }
            CopilotCommand::Status => {
                let (token, source) = joey_providers::copilot::resolve_copilot_token()?;
                if token.is_empty() {
                    println!("GitHub Copilot: not authenticated");
                    Ok(1)
                } else {
                    println!("GitHub Copilot: authenticated ({})", source);
                    Ok(0)
                }
            }
            CopilotCommand::Logout => {
                // Joey only owns COPILOT_GITHUB_TOKEN. Never clear GH_TOKEN or
                // GITHUB_TOKEN: those may belong to unrelated GitHub tooling.
                let variable = "COPILOT_GITHUB_TOKEN";
                let removed = std::env::var(variable)
                    .map(|v| !v.trim().is_empty())
                    .unwrap_or(false);
                save_env_value(variable, "")?;
                std::env::remove_var(variable);
                if removed {
                    println!("Removed GitHub Copilot credentials stored by Joey.");
                } else {
                    println!("No Joey-managed GitHub Copilot token was stored.");
                }
                println!("Note: `gh auth token` credentials remain managed by the GitHub CLI.");
                Ok(0)
            }
        },
    }
}
