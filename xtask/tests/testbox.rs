//! The test box, every phase, as `cargo testbox test` runs it: `cargo test -p xtask -- --ignored`.
//! What the xtask prints is passed on line by line through the test's own output, which the
//! harness captures as any test's: shown if it fails, or as it goes with `--nocapture`.

use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};

#[test]
#[ignore = "the test box: rootless Podman, minutes, an hour when images rebuild; removes the box's volumes"]
fn testbox() {
    let mut xtask = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("test")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the xtask starts");
    let stdout = xtask.stdout.take().expect("the xtask's stdout is piped");
    let stderr = xtask.stderr.take().expect("the xtask's stderr is piped");
    std::thread::scope(|scope| {
        scope.spawn(|| lines(stderr, |line| eprintln!("{line}")));
        lines(stdout, |line| println!("{line}"));
    });
    let status = xtask.wait().expect("the xtask ends");
    assert!(status.success(), "cargo testbox test: {status}");
}

/// Hands each line `output` gives to `pass`, until it ends.
fn lines(output: impl Read, pass: impl Fn(&str)) {
    for line in BufReader::new(output).lines() {
        pass(&line.expect("the xtask's output reads"));
    }
}
