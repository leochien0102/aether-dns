mod config;
mod dns;
mod server;
mod upstream;

use std::process::ExitCode;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match std::env::args_os().skip(1).collect::<Vec<_>>().as_slice() {
        [arg] if arg == "-h" || arg == "--help" => {
            println!("usage: aether-dns [-h|--help] [-V|--version] [-C <path>]");
            ExitCode::SUCCESS
        }
        [arg] if arg == "-V" || arg == "--version" => {
            println!("aether-dns 2026.11.04");
            ExitCode::SUCCESS
        }
        [arg, path] if arg == "-C" => {
            let result = match config::Config::read(std::path::Path::new(path)) {
                Ok(config) => server::run(config).await,
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
