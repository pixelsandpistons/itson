//! Exercise the actual executable with a new, isolated data directory.

use itson_protocol::UserId;
use itson_server::sqlite::SqliteStore;
use itson_server::store::Store;
use serde_json::{Value, json};
use std::error::Error;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[derive(Debug)]
struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    fn new() -> io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..128 {
            let ordinal = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "itson-binary-tests-{}-{ordinal}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not create an isolated binary test directory",
        ))
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[derive(Debug)]
struct RunningServer {
    child: Child,
    lines: Receiver<io::Result<String>>,
    reader: Option<JoinHandle<()>>,
}

impl RunningServer {
    fn start(data_dir: &Path) -> io::Result<Self> {
        let child = Command::new(env!("CARGO_BIN_EXE_itson-server"))
            .arg("--data-dir")
            .arg(data_dir)
            .args(["--listen", "127.0.0.1:0"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let (sender, lines) = mpsc::channel();
        // Own the child before any fallible pipe or reader setup, so an early
        // error still kills and waits for it.
        let mut server = Self {
            child,
            lines,
            reader: None,
        };
        let stdout = server
            .child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("the test server has no stdout pipe"))?;
        server.reader = Some(
            thread::Builder::new()
                .name("itson-binary-test-output".into())
                .spawn(move || {
                    for line in BufReader::new(stdout).lines() {
                        if sender.send(line).is_err() {
                            break;
                        }
                    }
                })?,
        );
        Ok(server)
    }

    fn startup(
        &self,
        setup_complete: bool,
    ) -> Result<(SocketAddr, Option<String>), Box<dyn Error>> {
        let mut lines = Vec::new();
        for _ in 0..8 {
            let line = self
                .lines
                .recv_timeout(Duration::from_secs(15))
                .map_err(|error| {
                    io::Error::other(format!("could not read server startup output: {error}"))
                })??;
            let finished =
                line.contains("plain HTTP") && line.contains("TLS or a TLS reverse proxy");
            lines.push(line);
            if finished {
                break;
            }
        }
        let address: SocketAddr = lines[0]
            .strip_prefix("itson is listening on http://")
            .and_then(|line| line.strip_suffix('.'))
            .ok_or_else(|| io::Error::other("the server did not print its listening address"))?
            .parse()?;
        assert!(address.ip().is_loopback());
        assert_ne!(
            address.port(),
            0,
            "startup must show the port actually bound"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("Data directory: "))
        );
        let expected = if setup_complete {
            "Setup is complete. Members can sign in."
        } else {
            "Setup has not run. Create the owner account with POST /api/setup."
        };
        assert!(lines.iter().any(|line| line == expected), "{lines:?}");
        assert!(lines.iter().any(|line| {
            line.contains("plain HTTP") && line.contains("TLS or a TLS reverse proxy")
        }));
        let codes: Vec<_> = lines
            .iter()
            .filter_map(|line| line.strip_prefix("Setup code: "))
            .collect();
        assert_eq!(codes.len(), usize::from(!setup_complete));
        let code = codes.first().map(|code| (*code).to_owned());
        if let Some(code) = &code {
            assert!(!code.is_empty());
            assert!(!code.chars().any(char::is_whitespace));
        }
        Ok((address, code))
    }
}

fn complete_setup(address: SocketAddr, setup_code: &str) -> Result<UserId, Box<dyn Error>> {
    let body = serde_json::to_vec(&json!({
        "server_name": "Fictional moon garden", "username": "moon_moth",
        "password": "fictional binary setup password", "setup_code": setup_code,
    }))?;
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    write!(
        stream,
        "POST /api/setup HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )?;
    stream.write_all(&body)?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let (head, body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| io::Error::other("setup returned no HTTP response"))?;
    assert!(head.starts_with("HTTP/1.1 201 "), "{response}");
    let body: Value = serde_json::from_str(body)?;
    assert!(!body.to_string().contains(setup_code));
    Ok(UserId(
        body["user_id"]
            .as_u64()
            .ok_or("setup did not return an owner ID")?,
    ))
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[test]
fn server_binary_creates_database_and_reports_setup_across_restarts() -> Result<(), Box<dyn Error>>
{
    let directory = TestDirectory::new()?;
    let data_dir = directory.path.join("nested fictional server data");
    let database = data_dir.join("itson.sqlite3");
    assert!(!data_dir.exists());
    let owner = {
        let server = RunningServer::start(&data_dir)?;
        let (address, setup_code) = server.startup(false)?;
        assert!(database.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&data_dir)?.permissions().mode() & 0o077, 0);
        }
        complete_setup(
            address,
            setup_code
                .as_deref()
                .ok_or("startup omitted the setup code")?,
        )?
    };
    {
        let server = RunningServer::start(&data_dir)?;
        let (address, setup_code) = server.startup(true)?;
        assert!(setup_code.is_none());
        drop(TcpStream::connect_timeout(
            &address,
            Duration::from_secs(2),
        )?);
    }
    {
        let store = SqliteStore::open(&database)?;
        let settings = store
            .settings()?
            .ok_or("setup settings did not survive restart")?;
        assert_eq!(settings.name, "Fictional moon garden");
        assert_eq!(settings.owner, owner);
        let account = store
            .account(owner)?
            .ok_or("owner did not survive restart")?;
        assert_eq!(account.username, "moon_moth");
        assert!(account.password_hash.starts_with("$pbkdf2-sha256$"));
    }
    Ok(())
}
