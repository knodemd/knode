//! `knode readwise`: the Readwise access token, kept in this machine's credential store.

use std::io::{BufRead, IsTerminal};

use anyhow::{Result, bail};
use clap::Subcommand;
use knode_config::Paths;
use knode_service::readwise;

/// What `knode readwise` does.
#[derive(Debug, Subcommand)]
pub enum Action {
    /// The access token from readwise.io/access_token.
    Token {
        #[command(subcommand)]
        action: TokenAction,
    },
}

/// What `knode readwise token` does.
#[derive(Debug, Subcommand)]
pub enum TokenAction {
    /// Keep a token, read from standard input, in the credential store (the Keychain on macOS).
    /// It is never written to a space.
    Set,
    /// Forget the token kept in the credential store.
    Delete,
}

/// Runs `knode readwise`.
pub fn run(paths: &Paths, action: Action) -> Result<()> {
    let Action::Token { action } = action;
    match action {
        TokenAction::Set => {
            let stdin = std::io::stdin();
            if stdin.is_terminal() {
                eprintln!(
                    "Paste your Readwise access token (readwise.io/access_token) and press Return:"
                );
            }
            let mut line = String::new();
            stdin.lock().read_line(&mut line)?;
            let token = line.trim();
            if token.is_empty() {
                bail!("no token given");
            }
            let place = readwise::set_token(&paths.home, token)?;
            println!("Kept the Readwise token in {place}.");
            if std::env::var(readwise::TOKEN_VAR).is_ok() {
                println!("{} is set too, and is used first.", readwise::TOKEN_VAR);
            }
        }
        TokenAction::Delete => {
            if readwise::delete_token(&paths.home)? {
                println!("Forgot the Readwise token.");
            } else {
                println!("No Readwise token was kept.");
            }
        }
    }
    Ok(())
}
