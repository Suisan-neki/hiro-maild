use std::{
    path::Path,
    process::{Command, Output},
};

fn cli(data: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hiro-maild"))
        .arg("--data-dir")
        .arg(data)
        .args(args)
        .env_remove("HIRO_MAILD_THUNDERBIRD_STORE")
        .output()
        .unwrap()
}

#[test]
fn cli_requires_start_position_and_dry_run_does_not_submit_mail() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let store = temp.path().join("store");
    std::fs::create_dir(&store).unwrap();
    std::fs::copy("tests/fixtures/sample.mbox", store.join("Inbox")).unwrap();
    std::fs::write(store.join("Inbox.msf"), b"").unwrap();
    assert!(!cli(&data, &["forward", "--dry-run"]).status.success());
    assert!(
        !cli(&data, &["forward-init", "--gmail", "test@gmail.com"])
            .status
            .success()
    );
    assert!(
        cli(&data, &["sync", "--store", store.to_str().unwrap()])
            .status
            .success()
    );
    assert!(
        cli(
            &data,
            &[
                "forward-init",
                "--gmail",
                "test@gmail.com",
                "--since",
                "2026-09-07T12:30:00+09:00"
            ]
        )
        .status
        .success()
    );
    let output = cli(&data, &["forward", "--dry-run"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let candidate: serde_json::Value =
        serde_json::from_str(stdout.lines().nth(1).unwrap()).unwrap();
    assert_eq!(candidate["id"], 2);
    assert_eq!(candidate["attachment_names"][0], "notice.txt");
    assert!(stdout.contains("mime id=2 valid=true"));
    let conn = rusqlite::Connection::open(data.join("hiro-maild.sqlite3")).unwrap();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM deliveries", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
