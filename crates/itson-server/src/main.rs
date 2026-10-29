//! Runs the HTTP and WebSocket server with a host-chosen SQLite data directory.

use itson_protocol::frame::ErrorCode;
use itson_server::http::{self, AppState};
use itson_server::password::Cost;
use std::ffi::OsString;
use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use tokio::net::TcpListener;

const HELP: &str = "itson-server --data-dir DIR --listen ADDR:PORT

Run the itson HTTP and WebSocket server.

  --data-dir DIR       Store data in DIR/itson.sqlite3; create DIR if needed.
  --listen ADDR:PORT   Listen on an IPv4 or bracketed IPv6 address and port.
  --help              Show this help.

Example: itson-server --data-dir ./itson-data --listen 127.0.0.1:8080

This server uses plain HTTP. Configure TLS or a TLS reverse proxy before
exposing it outside your machine.";

#[derive(Debug, PartialEq, Eq)]
struct Config {
    data_dir: PathBuf,
    listen: SocketAddr,
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help,
    Run(Config),
}

#[derive(Debug)]
struct StartupError {
    code: ErrorCode,
    detail: String,
}

impl StartupError {
    fn arguments(detail: &str) -> Self {
        Self {
            code: ErrorCode::Malformed,
            detail: format!("{detail} Use --help for the command format."),
        }
    }

    fn internal(detail: String) -> Self {
        Self {
            code: ErrorCode::Internal,
            detail,
        }
    }
}

impl fmt::Display for StartupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for StartupError {}

fn parse_arguments(arguments: impl IntoIterator<Item = OsString>) -> Result<Command, StartupError> {
    let mut arguments = arguments.into_iter();
    let mut data_dir = None;
    let mut listen = None;
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--help") => return Ok(Command::Help),
            Some("--data-dir") => {
                if data_dir.is_some() {
                    return Err(StartupError::arguments("Give --data-dir only once."));
                }
                let value = argument_value(&mut arguments, "Give a directory after --data-dir.")?;
                data_dir = Some(PathBuf::from(value));
            }
            Some("--listen") => {
                if listen.is_some() {
                    return Err(StartupError::arguments("Give --listen only once."));
                }
                let value = argument_value(&mut arguments, "Give an address after --listen.")?;
                let address = value
                    .to_str()
                    .and_then(|value| value.parse::<SocketAddr>().ok())
                    .ok_or_else(|| {
                        StartupError::arguments(
                            "Use an IPv4 or bracketed IPv6 address and port for --listen.",
                        )
                    })?;
                listen = Some(address);
            }
            _ => return Err(StartupError::arguments("That argument is not supported.")),
        }
    }
    let data_dir = data_dir.ok_or_else(|| StartupError::arguments("Give --data-dir DIR."))?;
    let listen = listen.ok_or_else(|| StartupError::arguments("Give --listen ADDR:PORT."))?;
    Ok(Command::Run(Config { data_dir, listen }))
}

fn argument_value(
    arguments: &mut impl Iterator<Item = OsString>,
    missing: &str,
) -> Result<OsString, StartupError> {
    match arguments.next() {
        Some(value) if !value.is_empty() && !value.to_string_lossy().starts_with("--") => Ok(value),
        _ => Err(StartupError::arguments(missing)),
    }
}

async fn run(arguments: impl IntoIterator<Item = OsString>) -> Result<(), StartupError> {
    let config = match parse_arguments(arguments)? {
        Command::Help => {
            println!("{HELP}");
            return Ok(());
        }
        Command::Run(config) => config,
    };
    create_data_dir(&config.data_dir).map_err(|error| {
        StartupError::internal(format!(
            "Could not create the data directory {}: {error}",
            config.data_dir.display()
        ))
    })?;
    let listener = TcpListener::bind(config.listen).await.map_err(|error| {
        StartupError::internal(format!("Could not listen on {}: {error}", config.listen))
    })?;
    let address = listener.local_addr().map_err(|error| {
        StartupError::internal(format!("Could not read the listening address: {error}"))
    })?;
    let database = config.data_dir.join("itson.sqlite3");
    let state = AppState::open(database, Cost::default())
        .await
        .map_err(|error| StartupError {
            code: error.code(),
            detail: format!("Could not open the server database: {error}"),
        })?;
    let setup_complete = state.setup_complete().await.map_err(|error| StartupError {
        code: error.code(),
        detail: format!("Could not read the server setup status: {error}"),
    })?;
    println!("itson is listening on http://{address}.");
    println!("Data directory: {}", config.data_dir.display());
    if setup_complete {
        println!("Setup is complete. Members can sign in.");
    } else {
        println!("Setup has not run. Create the owner account with POST /api/setup.");
    }
    println!(
        "This server uses plain HTTP. Configure TLS or a TLS reverse proxy before exposing it."
    );
    let server_result = axum::serve(listener, http::router(state.clone()))
        .with_graceful_shutdown(shutdown_signal())
        .await;
    // Let the worker finish accepted operations and close SQLite before exit,
    // including when the HTTP listener stopped with an error.
    let shutdown_result = state.shutdown().await;
    server_result
        .map_err(|error| StartupError::internal(format!("The HTTP server stopped: {error}")))?;
    shutdown_result.map_err(|error| StartupError {
        code: error.code(),
        detail: format!("Could not finish closing the server database: {error}"),
    })
}

fn create_data_dir(path: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        eprintln!("Could not wait for Ctrl-C: {error}. Stopping the server.");
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(std::env::args_os().skip(1)).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("itson-server: {error} [{:?}]", error.code);
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> Result<Command, StartupError> {
        parse_arguments(arguments.iter().map(OsString::from))
    }

    #[test]
    fn accepts_a_data_directory_with_spaces_and_an_ephemeral_ipv6_port() {
        assert_eq!(
            parse(&["--data-dir", "fictional moon garden", "--listen", "[::1]:0"]).unwrap(),
            Command::Run(Config {
                data_dir: PathBuf::from("fictional moon garden"),
                listen: "[::1]:0".parse().unwrap(),
            })
        );
    }

    #[test]
    fn accepts_arguments_in_either_order() {
        assert_eq!(
            parse(&["--listen", "127.0.0.1:8080", "--data-dir", "fictional-data"]).unwrap(),
            Command::Run(Config {
                data_dir: PathBuf::from("fictional-data"),
                listen: "127.0.0.1:8080".parse().unwrap(),
            })
        );
    }

    #[test]
    fn help_does_not_require_server_arguments() {
        assert_eq!(parse(&["--help"]).unwrap(), Command::Help);
    }

    #[test]
    fn rejects_missing_repeated_and_unknown_arguments() {
        for arguments in [
            vec![],
            vec!["--data-dir", "fictional-data"],
            vec!["--listen", "127.0.0.1:8080"],
            vec!["--data-dir"],
            vec!["--listen"],
            vec!["--data-dir", "--listen", "127.0.0.1:8080"],
            vec!["--data-dir", ""],
            vec![
                "--data-dir",
                "fictional-data",
                "--data-dir",
                "other-fictional-data",
            ],
            vec!["--listen", "127.0.0.1:8080", "--listen", "127.0.0.1:8081"],
            vec!["--unsupported"],
        ] {
            let error = parse(&arguments).unwrap_err();
            assert_eq!(error.code, ErrorCode::Malformed);
            assert!(error.to_string().contains("--help"));
        }
    }

    #[test]
    fn rejects_an_invalid_listening_address() {
        let error = parse(&[
            "--data-dir",
            "fictional-data",
            "--listen",
            "invalid-address",
        ])
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::Malformed);
        assert!(error.to_string().contains("address and port"));
    }
}
