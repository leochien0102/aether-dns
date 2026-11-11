mod config;
mod dns;
mod lists;
mod server;
mod upstream;
mod upstream_group;

use std::process::ExitCode;
use tokio::signal::unix::{SignalKind, signal};

async fn serve(config: config::Config) -> Result<(), Box<dyn std::error::Error>> {
    // Register both handlers before the server emits its readiness lines.
    let mut term = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let (shutdown, receiver) = tokio::sync::watch::channel(false);
    let signals = tokio::spawn(async move {
        tokio::select! {
            _ = term.recv() => {},
            _ = interrupt.recv() => {},
        }
        shutdown.send_replace(true);
        tokio::select! {
            _ = term.recv() => {},
            _ = interrupt.recv() => {},
        }
        // A second TERM or INT skips draining, including runtime teardown.
        std::process::exit(1);
    });
    let result = server::run(config, receiver).await;
    signals.abort();
    result
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match std::env::args_os().skip(1).collect::<Vec<_>>().as_slice() {
        [arg] if arg == "-h" || arg == "--help" => {
            println!("usage: aether-dns [-h|--help] [-V|--version] [-C <path>]");
            ExitCode::SUCCESS
        }
        [arg] if arg == "-V" || arg == "--version" => {
            println!("aether-dns {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        [arg, path] if arg == "-C" => {
            let result = match config::Config::read(std::path::Path::new(path)) {
                Ok(config) => serve(config).await,
                Err(error) => Err(error.into()),
            };
            match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("{error}");
                    ExitCode::from(1)
                }
            }
        }
        _ => {
            eprintln!("usage: aether-dns [-h|--help] [-V|--version] [-C <path>]");
            ExitCode::from(2)
        }
    }
}
