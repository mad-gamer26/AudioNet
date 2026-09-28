//! `audionet-server`: the self-hostable AudioNet coordination server.

#![forbid(unsafe_code)]

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use audionet_server::api::{AppState, router};
use audionet_server::auth;
use audionet_server::config::Config;
use audionet_server::db::{self, Db};
use audionet_server::mail::{Mailer, validate_email};
use audionet_server::security_headers;
use clap::{Parser, Subcommand};
use tower_http::services::{ServeDir, ServeFile};

#[derive(Debug, Parser)]
#[command(name = "audionet-server", bin_name = "audionet-server", version)]
/// AudioNet coordination server: accounts, device sign-in, presence and signaling.
struct Cli {
    /// Configuration file.
    #[arg(
        long,
        short,
        global = true,
        default_value = "config.toml",
        env = "AUDIONET_CONFIG"
    )]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the server.
    Serve,
    /// Check the configuration file and print the effective settings.
    CheckConfig,
    /// Manage user accounts.
    User {
        #[command(subcommand)]
        action: UserAction,
    },
}

#[derive(Debug, Subcommand)]
enum UserAction {
    /// Create a user. The password is read from standard input.
    Add {
        username: String,
        /// The user's email address, for password resets (recorded as
        /// confirmed).
        #[arg(long)]
        email: Option<String>,
    },
    /// Change a user's password (signs out their web sessions).
    Passwd { username: String },
    /// Delete a user and their devices.
    Delete { username: String },
    /// Set a user's email address, recorded as confirmed (the
    /// administrator vouches for it), or remove it with --remove.
    Email {
        username: String,
        #[arg(required_unless_present = "remove", conflicts_with = "remove")]
        address: Option<String>,
        #[arg(long)]
        remove: bool,
    },
    /// List users and their email addresses.
    List,
}

fn read_password() -> Result<String, String> {
    eprint!("Password (at least 10 characters), then Enter: ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| format!("could not read the password: {e}"))?;
    let password = line.trim_end_matches(['\r', '\n']).to_owned();
    auth::validate_password(&password).map_err(str::to_owned)?;
    Ok(password)
}

fn user_command(config: &Config, action: UserAction) -> Result<(), String> {
    let db = Db::open(&config.database).map_err(|e| format!("could not open the database: {e}"))?;
    match action {
        UserAction::Add { username, email } => {
            auth::validate_username(&username).map_err(str::to_owned)?;
            let email = email
                .map(|e| validate_email(&e).map_err(str::to_owned))
                .transpose()?;
            if let Some(e) = &email
                && db
                    .with(|c| db::email_taken(c, e, 0))
                    .map_err(|e| e.to_string())?
            {
                return Err(format!("another account already uses {e}"));
            }
            let hash = auth::hash_password(&read_password()?)?;
            db.with(|c| {
                let id = db::create_user(c, &username, &hash)?;
                if let Some(e) = &email {
                    db::set_email(c, id, Some(e), true)?;
                }
                Ok(())
            })
            .map_err(|e| format!("could not create {username}: {e}"))?;
            println!("Created user {username}.");
        }
        UserAction::Email {
            username,
            address,
            remove,
        } => {
            let user = db
                .with(|c| db::user_by_name(c, &username))
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("no user named {username}"))?;
            if remove {
                db.with(|c| db::set_email(c, user.id, None, false))
                    .map_err(|e| e.to_string())?;
                println!("Removed the email address of {}.", user.username);
            } else {
                let email = validate_email(address.as_deref().unwrap_or_default())
                    .map_err(str::to_owned)?;
                if db
                    .with(|c| db::email_taken(c, &email, user.id))
                    .map_err(|e| e.to_string())?
                {
                    return Err(format!("another account already uses {email}"));
                }
                db.with(|c| db::set_email(c, user.id, Some(&email), true))
                    .map_err(|e| e.to_string())?;
                println!(
                    "Set the email address of {} to {email} (confirmed).",
                    user.username
                );
            }
        }
        UserAction::Passwd { username } => {
            let user = db
                .with(|c| db::user_by_name(c, &username))
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("no user named {username}"))?;
            let hash = auth::hash_password(&read_password()?)?;
            db.with(|c| db::set_password(c, user.id, &hash))
                .map_err(|e| e.to_string())?;
            println!("Changed the password for {username} and signed out their web sessions.");
        }
        UserAction::Delete { username } => {
            if db
                .with(|c| db::delete_user(c, &username))
                .map_err(|e| e.to_string())?
            {
                println!("Deleted {username} and their devices.");
            } else {
                return Err(format!("no user named {username}"));
            }
        }
        UserAction::List => {
            let users = db.with(db::list_users).map_err(|e| e.to_string())?;
            println!("{} users.", users.len());
            for u in users {
                match (&u.email, u.email_verified) {
                    (Some(e), true) => println!("{}: email {e}, confirmed", u.username),
                    (Some(e), false) => println!("{}: email {e}, not confirmed", u.username),
                    (None, _) => println!("{}: no email address", u.username),
                }
            }
        }
    }
    Ok(())
}

async fn serve(config: Config) -> Result<(), String> {
    let db = Db::open(&config.database).map_err(|e| {
        format!(
            "could not open the database {}: {e}",
            config.database.display()
        )
    })?;
    if db.with(db::user_count).unwrap_or(0) == 0 && !config.allow_registration {
        tracing::warn!("no user accounts exist; create one with `audionet-server user add NAME`");
    }
    let bind = config.bind;
    let web_root = config.web_root.clone();
    let downloads = config.downloads_dir.clone();
    let mailer = Mailer::from_config(&config.email)?;
    if !mailer.enabled() {
        tracing::warn!(
            "no [email] settings: addresses cannot be confirmed and password reset is off"
        );
    }
    let state = Arc::new(AppState::new(config, db, mailer));
    let mut app = router(state);
    if let Some(dir) = downloads {
        app = app.nest_service("/downloads", ServeDir::new(dir));
    }
    if let Some(root) = web_root {
        let index = root.join("index.html");
        app = app.fallback_service(ServeDir::new(&root).not_found_service(ServeFile::new(index)));
    }
    let app = app.layer(axum::middleware::from_fn(security_headers));
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|e| format!("could not listen on {bind}: {e}"))?;
    tracing::info!("AudioNet server listening on {bind}");
    // Connection addresses feed the sign-up limit when no proxy header is
    // configured.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
    .map_err(|e| e.to_string())
}

fn main() -> ExitCode {
    use std::io::IsTerminal;
    tracing_subscriber::fmt()
        // Plain text for journald and log files; color only on a terminal.
        .with_ansi(std::io::stderr().is_terminal())
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "audionet_server=info,tower_http=warn".into()),
        )
        .init();
    let cli = Cli::parse();
    let loaded = match cli.command {
        Command::User { .. } => Config::load_for_admin(&cli.config),
        _ => Config::load(&cli.config),
    };
    let config = match loaded {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result = match cli.command {
        Command::CheckConfig => {
            println!("Configuration is valid.");
            println!("Public URL: {}", config.public_url);
            println!("Listening address: {}", config.bind);
            println!("Database: {}", config.database.display());
            println!(
                "Web client directory: {}",
                config
                    .web_root
                    .as_ref()
                    .map_or("none (API only)".into(), |p| p.display().to_string())
            );
            println!(
                "Downloads directory: {}",
                config
                    .downloads_dir
                    .as_ref()
                    .map_or("none".into(), |p| p.display().to_string())
            );
            println!("Allowed origins: {}", config.allowed_origins.join(", "));
            println!(
                "Open registration: {}",
                if config.allow_registration {
                    "yes"
                } else {
                    "no"
                }
            );
            match (&config.email.from, &config.email.smtp_host) {
                (Some(from), Some(host)) => {
                    if let Err(e) = Mailer::check_config(&config.email) {
                        return {
                            eprintln!("Error: {e}");
                            ExitCode::FAILURE
                        };
                    }
                    println!(
                        "Email: from {from} through {host} port {} ({})",
                        config.email.port(),
                        match config.email.smtp_security {
                            audionet_server::config::SmtpSecurity::Starttls => "STARTTLS",
                            audionet_server::config::SmtpSecurity::Tls => "TLS",
                            audionet_server::config::SmtpSecurity::None => "no encryption",
                        }
                    );
                }
                _ => println!("Email: off (no address confirmation or password reset)"),
            }
            println!("STUN servers: {}", config.ice.stun_urls.len());
            println!(
                "TURN servers: {} ({})",
                config.ice.turn_urls.len(),
                if config.ice.turn_secret.is_some() {
                    "secret set"
                } else {
                    "no secret"
                }
            );
            Ok(())
        }
        Command::User { action } => user_command(&config, action),
        Command::Serve => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build();
            match rt {
                Ok(rt) => rt.block_on(serve(config)),
                Err(e) => Err(e.to_string()),
            }
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::FAILURE
        }
    }
}
