//! FoundationDB for the box, the service Signal-Server's `foundationDb.serviceContainerNamePrefix`
//! names (LocalFoundationDbDatabaseFactory.java:29): the foundationdb image's start (its
//! /var/fdb/scripts/fdb.bash) for one process in fdb.bash's `host` mode, which advertises
//! 127.0.0.1, as every container in the pod shares localhost, plus the `configure new single
//! memory` Signal-Server's CI runs once the database answers (.github/workflows/test.yml).

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

use std::path::PathBuf;
use std::time::Duration;

use testbox_tools::process::supervise;
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

/// What keeps the database from starting or from being configured.
#[derive(Debug, thiserror::Error)]
enum FoundationDbError {
    #[error("usage: foundationdb, not {argument}")]
    Argument { argument: String },
    #[error("creating {}", path.display())]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("starting fdbserver")]
    StartServer(#[source] std::io::Error),
    #[error("running fdbcli --exec {command:?}")]
    Fdbcli {
        command: String,
        #[source]
        source: std::io::Error,
    },
    /// fdbcli's answer to `configure`, neither a new database nor one there already.
    #[error("{command}: {answer}")]
    Configure { command: String, answer: String },
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let outcome = match testbox_tools::argument("foundationdb") {
        Ok(None) => run().await,
        Ok(Some(argument)) => Err(FoundationDbError::Argument { argument }.into()),
        Err(error) => Err(error.into()),
    };
    match outcome {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("foundationdb: {}", testbox_tools::Report(&*error));
            std::process::ExitCode::FAILURE
        }
    }
}

/// What fdbcli printed. Its exit status isn't checked: an unavailable database and a database
/// that exists already are both answers here, which only the text tells apart.
async fn fdbcli(command: &str) -> Result<String, FoundationDbError> {
    let timeout = CONFIGURE_TIMEOUT.as_secs().to_string();
    let output = Command::new("fdbcli")
        .args(["--cluster-file", CLUSTER.file])
        .args(["--timeout", &timeout, "--exec", command])
        .output()
        .await
        .map_err(|source| FoundationDbError::Fdbcli {
            command: command.to_owned(),
            source,
        })?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let hostname = testbox_tools::read("/proc/sys/kernel/hostname".as_ref())?
        .trim()
        .to_owned();
    for dir in ["/var/fdb/data", "/var/fdb/logs"] {
        std::fs::create_dir_all(dir).map_err(|source| FoundationDbError::CreateDirectory {
            path: dir.into(),
            source,
        })?;
    }
    testbox_tools::write(
        CLUSTER.file.as_ref(),
        format!("docker:docker@{}\n", CLUSTER.address),
    )?;

    let mut server = Command::new("fdbserver")
        .args(["--cluster-file", CLUSTER.file])
        .args(["--listen-address", CLUSTER.address])
        .args(["--public-address", CLUSTER.address])
        .args(["--datadir", "/var/fdb/data", "--logdir", "/var/fdb/logs"])
        .arg(format!("--locality-zoneid={hostname}"))
        .arg(format!("--locality-machineid={hostname}"))
        .kill_on_drop(true)
        .spawn()
        .map_err(FoundationDbError::StartServer)?;

    // fdbcli waits for the database to answer, up to its timeout: no polling here.
    const CONFIGURE: &str = "configure new single memory";
    tokio::select! {
        configured = fdbcli(CONFIGURE) => {
            let configured = configured?;
            if !configured.contains("Database created") && !configured.contains("already exists") {
                return Err(FoundationDbError::Configure {
                    command: CONFIGURE.to_owned(),
                    answer: configured,
                }
                .into());
            }
        }
        supervised = supervise(&mut server, "fdbserver") => {
            // Stopped or exited before the database was configured.
            return Ok(supervised?);
        }
    }
    Ok(supervise(&mut server, "fdbserver").await?)
}
