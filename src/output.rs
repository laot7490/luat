use std::io::IsTerminal;

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

pub fn stderr_message(message: &str) -> String {
    style_message(
        message,
        pretty() && std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
    )
}

pub(crate) fn warning(message: &str) {
    eprintln!("\n{} {message}", paint("warning:", "33"));
}

pub(crate) fn paint(text: &str, color: &str) -> String {
    if pretty() && std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none() {
        format!("\x1b[{color}m{text}\x1b[0m")
    } else {
        text.to_owned()
    }
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
