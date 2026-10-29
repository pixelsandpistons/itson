//! Phase 0 voice spike: Opus over QUIC datagrams through a self-hosted server.
//!
//! See `spikes/voice-quic/README.md` for the measurement procedure.

mod probe;
mod server;
mod signal;
mod talk;
mod tls;

use std::collections::HashMap;
use std::path::PathBuf;
use tls::BoxError;

const USAGE: &str = "\
usage:
  itson-voice-spike server  --listen ADDR:PORT --data-dir DIR [--echo]
  itson-voice-spike probe   --server HOST:PORT --fingerprint HEX [--seconds 30]
                            [--drop-percent 0] [--buffer-frames 3] [--bitrate 32000] [--json FILE]
  itson-voice-spike talk    --server HOST:PORT --fingerprint HEX [--input ID] [--output ID]
                            [--buffer-frames 3] [--bitrate 32000]
  itson-voice-spike devices";

struct Args {
    values: HashMap<String, String>,
    flags: Vec<String>,
}

impl Args {
    fn parse(rest: &[String]) -> Result<Self, BoxError> {
        let (mut values, mut flags) = (HashMap::new(), Vec::new());
        let mut iter = rest.iter().peekable();
        while let Some(arg) = iter.next() {
            let key = arg
                .strip_prefix("--")
                .ok_or_else(|| format!("unexpected argument {arg}"))?;
            match iter.peek() {
                Some(next) if !next.starts_with("--") => {
                    values.insert(key.to_owned(), (*next).clone());
                    iter.next();
                }
                _ => flags.push(key.to_owned()),
            }
        }
        Ok(Args { values, flags })
    }

    fn required(&self, key: &str) -> Result<String, BoxError> {
        self.values
            .get(key)
            .cloned()
            .ok_or_else(|| format!("--{key} is required\n{USAGE}").into())
    }

    fn optional(&self, key: &str) -> Option<String> {
        self.values.get(key).cloned()
    }

    fn number<T: std::str::FromStr>(&self, key: &str, default: T) -> Result<T, BoxError>
    where
        T::Err: std::error::Error + Send + Sync + 'static,
    {
        match self.values.get(key) {
            Some(v) => Ok(v.parse()?),
            None => Ok(default),
        }
    }

    fn flag(&self, key: &str) -> bool {
        self.flags.iter().any(|f| f == key)
    }
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), BoxError> {
    let all: Vec<String> = std::env::args().skip(1).collect();
    let Some((command, rest)) = all.split_first() else {
        println!("{USAGE}");
        return Ok(());
    };
    let args = Args::parse(rest)?;
    match command.as_str() {
        "server" => {
            server::run(
                args.required("listen")?.parse()?,
                PathBuf::from(args.required("data-dir")?),
                args.flag("echo"),
            )
            .await
        }
        "probe" => {
            probe::run(probe::ProbeArgs {
                server: args.required("server")?,
                fingerprint: args.required("fingerprint")?,
                seconds: args.number("seconds", 30)?,
                drop_percent: args.number("drop-percent", 0.0)?,
                target_frames: args.number("buffer-frames", 3)?,
                bitrate: args.number("bitrate", 32_000)?,
                json: args.optional("json").map(PathBuf::from),
            })
            .await
        }
        "talk" => {
            talk::run(talk::TalkArgs {
                server: args.required("server")?,
                fingerprint: args.required("fingerprint")?,
                input: args.optional("input"),
                output: args.optional("output"),
                target_frames: args.number("buffer-frames", 3)?,
                bitrate: args.number("bitrate", 32_000)?,
            })
            .await
        }
        "devices" => talk::list_devices(),
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            Ok(())
        }
        other => Err(format!("unknown command {other}\n{USAGE}").into()),
    }
}
