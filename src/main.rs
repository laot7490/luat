use std::process::ExitCode;

mod cli;
mod output;

fn main() -> ExitCode {
    match cli::run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, message)) => {
            eprintln!("{message}");
            ExitCode::from(code)
        }
    }
}
