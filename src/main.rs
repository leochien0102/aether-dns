mod dns;

use std::process::ExitCode;

fn main() -> ExitCode {
    eprintln!("usage: aether-dns");
    ExitCode::from(2)
}
