use std::io::{self, IsTerminal, Write};

#[doc(hidden)]
pub fn read_prompt(label: &str, default: Option<&str>) -> io::Result<Option<String>> {
    {
        let mut stdout = io::stdout().lock();
        write!(stdout, "{label}")?;
        if let Some(default) = default {
            write!(stdout, " ({default})")?;
        }
        write!(stdout, ": ")?;
        stdout.flush()?;
    }
    let mut value = String::new();
    if io::stdin().read_line(&mut value)? == 0 {
        return Ok(None);
    }
    let value = value.trim();
    Ok(Some(if value.is_empty() {
        default.unwrap_or("").to_owned()
    } else {
        value.to_owned()
    }))
}

thread_local! { static PRETTY: std::cell::Cell<bool> = const { std::cell::Cell::new(true) }; }

pub fn pretty() -> bool {
    PRETTY.get()
}

pub fn with_pretty<T>(enabled: bool, action: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            PRETTY.set(self.0);
        }
    }
    let _restore = Restore(PRETTY.replace(enabled));
    action()
}

pub fn style_message(message: &str, color: bool) -> String {
    if !color {
        return message.to_owned();
    }
    message
        .split('\n')
        .map(|line| {
            let text = line.trim_start();
            let indent = &line[..line.len() - text.len()];
            for (label, code) in [
                ("error", "31"),
                ("warning", "33"),
                ("help:", "36"),
                ("note:", "36"),
            ] {
                if let Some(rest) = text.strip_prefix(label)
                    && (label.ends_with(':') || rest.starts_with(' ') || rest.starts_with(':'))
                {
                    return format!("{indent}\x1b[{code}m{label}\x1b[0m{rest}");
                }
            }
            line.to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn color_enabled(terminal: bool) -> bool {
    pretty() && terminal && std::env::var_os("NO_COLOR").is_none()
}

pub fn log_line(level: &str, message: &str, terminal: bool) -> String {
    let tag = concat!("luat v", env!("CARGO_PKG_VERSION"));
    if color_enabled(terminal) {
        let color = match level {
            "ERROR" => 31,
            "WARNING" => 33,
            "SUCCESS" => 32,
            _ => return format!("\x1b[1;38;2;157;130;241m{tag}\x1b[0m {message}"),
        };
        format!("\x1b[1;38;2;157;130;241m{tag}\x1b[0m \x1b[{color}m{message}\x1b[0m")
    } else {
        format!("{tag} {message}")
    }
}

pub fn status(level: &str, message: &str) {
    println!(
        "{}",
        log_line(level, message, std::io::stdout().is_terminal())
    );
}

pub fn stderr_message(message: &str) -> String {
    let message = message.trim();
    let terminal = std::io::stderr().is_terminal();
    if let Some(message) = message.strip_prefix("error ") {
        return log_line("ERROR", message, terminal);
    }
    if let Some(message) = message.strip_prefix("warning ") {
        return log_line("WARNING", message, terminal);
    }
    style_message(
        message,
        pretty() && std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
    )
}

pub fn warning(message: &str) {
    stderr("WARNING", message);
}

pub fn stderr(level: &str, message: &str) {
    eprintln!(
        "{}",
        log_line(level, message, std::io::stderr().is_terminal())
    );
}

#[cfg(test)]
mod tests {
    #[test]
    fn plain_scope_restores_preference_after_unwinding() {
        let _ = std::panic::catch_unwind(|| {
            super::with_pretty(false, || {
                assert!(!super::pretty());
                panic!("test");
            })
        });
        assert!(super::pretty());
    }

    #[test]
    fn styles_only_diagnostic_labels() {
        let text = "error LT5001: Unknown command\n  help: Run luat --help";
        assert_eq!(super::style_message(text, false), text);
        let styled = super::style_message(text, true);
        assert!(styled.contains("\x1b[31merror\x1b[0m LT5001:"));
        assert!(styled.contains("\x1b[36mhelp:\x1b[0m"));
    }
}
