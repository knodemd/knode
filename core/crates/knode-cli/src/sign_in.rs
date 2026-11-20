//! Signing a connector in to the account it reads, step by step through Knode Service: the
//! phone number, the code the service sent, and the password if the account has one. The
//! session stays in the service, which keeps it only in the system keychain.

use std::io::{BufRead, IsTerminal, Write};

use anyhow::{Result, bail};
use knode_service::Client;
use knode_service::sources::{SignInState, SignInStep};

/// Signs `source` in unless it already is.
pub fn if_needed(client: &Client, source: &str) -> Result<()> {
    if let SignInState::SignedIn { account } = client.sources_account(source)? {
        println!("{source} is signed in as {account}.");
        return Ok(());
    }
    run(client, source)
}

/// Signs `source` in, asking for each step.
pub fn run(client: &Client, source: &str) -> Result<()> {
    let mut state = client.sources_account(source)?;
    if matches!(state, SignInState::SignedIn { .. }) {
        state = SignInState::SignedOut;
    }
    loop {
        let step = match &state {
            SignInState::SignedIn { account } => {
                println!("Signed {source} in as {account}. The session is kept in the keychain.");
                return Ok(());
            }
            SignInState::SignedOut => SignInStep::Start {
                account: match source {
                    // The workspace is picked on Slack's page.
                    "slack" => String::new(),
                    "gmail" => {
                        ask_optional("Your Gmail address (Return to choose it on Google's page)")?
                    }
                    "imap" => ask(
                        "Your mail address, then the IMAP server unless it is Gmail, iCloud, \
                         Outlook, Yahoo or Fastmail (you@example.com imap.example.com)",
                    )?,
                    _ => ask("Phone number of the account, in international format (+34...)")?,
                },
            },
            SignInState::OpenBrowser { url } => {
                println!("Sign in on this page, in your browser:\n\n  {url}\n");
                open_in_browser(url);
                ask_optional("Press Return once the browser says you are signed in")?;
                SignInStep::Browser
            }
            SignInState::CodeSent { via } => SignInStep::Code {
                code: ask(&match via {
                    Some(via) => format!("The code sent to {via}"),
                    None => "The code Telegram sent".to_string(),
                })?,
            },
            SignInState::PasswordNeeded { hint } => SignInStep::Password {
                password: ask_hidden(&match hint {
                    Some(hint) if source == "imap" => format!("App password ({hint})"),
                    Some(hint) if !hint.is_empty() => format!("Password (hint: {hint})"),
                    _ => "Password".to_string(),
                })?,
            },
        };
        state = client.sources_sign_in(source, step)?;
    }
}

/// Asks for a line that may be empty.
fn ask_optional(prompt: &str) -> Result<String> {
    eprint!("{prompt}: ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line)? == 0 {
        bail!("signing in stopped: nothing was given");
    }
    Ok(line.trim().to_string())
}

/// Opens `url` in the browser of this machine, when the command runs in a terminal.
fn open_in_browser(url: &str) {
    if !std::io::stdout().is_terminal() {
        return;
    }
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(program)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn ask(prompt: &str) -> Result<String> {
    eprint!("{prompt}: ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line)? == 0 {
        bail!("signing in stopped: nothing was given");
    }
    let line = line.trim().to_string();
    if line.is_empty() {
        bail!("signing in stopped: nothing was given");
    }
    Ok(line)
}

/// Asks without showing what is typed, on a terminal.
fn ask_hidden(prompt: &str) -> Result<String> {
    let terminal = std::io::stdin().is_terminal();
    let echo = |on: bool| {
        if terminal {
            let _ = std::process::Command::new("stty")
                .arg(if on { "echo" } else { "-echo" })
                .stdin(std::process::Stdio::inherit())
                .status();
        }
    };
    echo(false);
    let answer = ask(prompt);
    echo(true);
    if terminal {
        eprintln!();
    }
    answer
}
