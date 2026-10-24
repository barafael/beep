//! FoundationDB for the box, the service Signal-Server's `foundationDb.serviceContainerNamePrefix`
//! names (LocalFoundationDbDatabaseFactory.java:29): the foundationdb image's start (its
//! /var/fdb/scripts/fdb.bash) for one process in fdb.bash's `host` mode, which advertises
//! 127.0.0.1, as every container in the pod shares localhost, plus the `configure new single
//! memory` Signal-Server's CI runs once the database answers (.github/workflows/test.yml).

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

use std::time::Duration;

use testbox_tools::process::supervise;
use testbox_tools::{Context, Error, Result, WithContext};
use tokio::process::Command;

/// The one-node cluster: its file, and the address of its one process, which the file names.
struct Cluster {
    file: &'static str,
    address: &'static str,
}

const CLUSTER: Cluster = Cluster {
    file: "/var/fdb/fdb.cluster",
    address: "127.0.0.1:4500",
};
/// How long fdbcli waits for the database to answer; it keeps trying meanwhile.
const CONFIGURE_TIMEOUT: Duration = Duration::from_secs(60);

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let outcome = match testbox_tools::argument("foundationdb") {
        Ok(None) => run().await,
        Ok(Some(other)) => Err(Error::Failed(format!("usage: foundationdb, not {other}"))),
        Err(error) => Err(error),
    };
    testbox_tools::exit("foundationdb", outcome)
}

/// What fdbcli printed. Its exit status isn't checked: an unavailable database and a database
/// that exists already are both answers here, which only the text tells apart.
async fn fdbcli(command: &str) -> Result<String> {
    let timeout = CONFIGURE_TIMEOUT.as_secs().to_string();
    let output = Command::new("fdbcli")
        .args(["--cluster-file", CLUSTER.file])
        .args(["--timeout", &timeout, "--exec", command])
        .output()
        .await
        .with_context(|| format!("fdbcli --exec {command:?}"))?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

async fn run() -> Result<()> {
    let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .context("this container's hostname")?
        .trim()
        .to_owned();
    for dir in ["/var/fdb/data", "/var/fdb/logs"] {
        std::fs::create_dir_all(dir).context(dir)?;
    }
    std::fs::write(CLUSTER.file, format!("docker:docker@{}\n", CLUSTER.address))
        .context(CLUSTER.file)?;

    let mut server = Command::new("fdbserver")
        .args(["--cluster-file", CLUSTER.file])
        .args(["--listen-address", CLUSTER.address])
        .args(["--public-address", CLUSTER.address])
        .args(["--datadir", "/var/fdb/data", "--logdir", "/var/fdb/logs"])
        .arg(format!("--locality-zoneid={hostname}"))
        .arg(format!("--locality-machineid={hostname}"))
        .kill_on_drop(true)
        .spawn()
        .context("fdbserver")?;

    // fdbcli waits for the database to answer, up to its timeout: no polling here.
    tokio::select! {
        configured = fdbcli("configure new single memory") => {
            let configured = configured?;
            if !configured.contains("Database created") && !configured.contains("already exists") {
                return Err(Error::Failed(format!("configure new single memory: {configured}")));
            }
        }
        supervised = supervise(&mut server, "fdbserver") => {
            // Stopped or exited before the database was configured.
            return supervised;
        }
    }
    supervise(&mut server, "fdbserver").await
}
