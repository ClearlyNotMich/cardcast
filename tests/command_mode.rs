//! Runs `cardcast` with arguments, as a Stream Deck key or a script does (SPEC section 11).
#![allow(
    clippy::unwrap_used,
    reason = "a test; clippy allows unwrap only inside #[test] functions"
)]

use std::io::Read as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The exit code and standard error; panics if cardcast hasn't exited after a minute (it opened its window).
fn cardcast(args: &[&str]) -> (Option<i32>, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_cardcast"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("cardcast {args:?} did not exit");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    (status.code(), stderr)
}

#[test]
fn a_wrong_grammar_exits_2_with_one_line() {
    let (code, stderr) = cardcast(&["input", "4GZY5P3", "toggle", "dp"]);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(
        stderr.starts_with("Not understood: toggle needs two inputs.")
            && stderr.lines().count() == 1,
        "{stderr}"
    );
}

#[test]
fn an_unknown_monitor_exits_1_with_one_line() {
    let (code, stderr) = cardcast(&["input", "no-such-monitor", "hdmi1"]);
    assert_eq!(code, Some(1), "{stderr}");
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
}
