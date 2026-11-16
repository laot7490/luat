use std::process::ExitCode;

mod cli;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let pretty = !args.windows(2).any(|pair| pair == ["--pretty", "false"]);
    luat::output::with_pretty(pretty, || match cli::run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, message)) => {
            eprintln!("{}", luat::output::stderr_message(&message));
            ExitCode::from(code)
        }
    })
}
