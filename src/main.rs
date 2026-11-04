use std::process::ExitCode;

fn main() -> ExitCode {
    match std::env::args_os().skip(1).collect::<Vec<_>>().as_slice() {
        [arg] if arg == "-h" || arg == "--help" => {
            println!("usage: aether-dns [-h|--help] [-V|--version]");
            ExitCode::SUCCESS
        }
        [arg] if arg == "-V" || arg == "--version" => {
            println!("aether-dns 2026.11.04");
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("usage: aether-dns [-h|--help] [-V|--version]");
            ExitCode::from(2)
        }
    }
}
