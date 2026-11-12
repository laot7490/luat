use std::io::IsTerminal;

pub(crate) fn warning(message: &str) {
    eprintln!("{} {message}", paint("warning:", "33"));
}

pub(crate) fn paint(text: &str, color: &str) -> String {
    if std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none() {
        format!("\x1b[{color}m{text}\x1b[0m")
    } else {
        text.to_owned()
    }
}
