use std::{
    fs,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

fn project() -> PathBuf {
    let id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::current_dir()
        .unwrap()
        .join("target")
        .join(format!("luat-cli-output-{id}"));
    fs::create_dir_all(root.join("src/server")).unwrap();
    fs::write(
        root.join("luat.toml"),
        "[project]\nname='test'\n[fivem]\nserver=['src/server/main.luat']\n",
    )
    .unwrap();
    fs::write(root.join("src/server/main.luat"), "local value: number = 1").unwrap();
    root
}

fn run(root: &PathBuf, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_luat"))
        .args(args)
        .arg("--project")
        .arg(root)
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

#[test]
fn starts_check_build_and_diagnostic_blocks_with_blank_lines() {
    let root = project();
    for (args, expected) in [
        (&["check"][..], "Checked 1 source file and 0 declarations"),
        (&["build"][..], "Build complete in"),
    ] {
        let output = run(&root, args);
        assert!(output.status.success());
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.starts_with('\n'));
        assert!(stdout.contains(expected));
    }

    fs::write(
        root.join("src/server/main.luat"),
        "local value: number = 'bad'",
    )
    .unwrap();
    for args in [&["check"][..], &["check", "--pretty", "false"][..]] {
        let output = run(&root, args);
        assert!(!output.status.success());
        assert!(String::from_utf8(output.stderr).unwrap().starts_with('\n'));
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn starts_configuration_errors_with_a_blank_line() {
    let root = project();
    fs::remove_file(root.join("luat.toml")).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_luat"))
        .args(["check", "--project"])
        .arg(&root)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr).unwrap().starts_with('\n'));
    fs::remove_dir_all(root).unwrap();
}
