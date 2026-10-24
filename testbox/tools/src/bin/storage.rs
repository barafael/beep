//! The storage service for the box: the Bigtable emulator storage-service's tests run
//! (google-cloud-bigtable-emulator, testbox/storage/Containerfile), its four tables created once
//! it listens, as those tests create them, then storage-service on it. One container, so that
//! storage-service starts once its tables exist: kube play has no readiness or startup probes.
//! The emulator keeps the tables in memory, so each start creates them anew.

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

use std::collections::HashMap;
use std::time::Duration;

use googleapis_tonic_google_bigtable_admin_v2::google::bigtable::admin::v2::{
    ColumnFamily, CreateTableRequest, Table, bigtable_table_admin_client::BigtableTableAdminClient,
};
use testbox_tools::keys::{BIGTABLE, KeyFile};
use testbox_tools::process::{spawn_logged, supervise, wait_for};
use testbox_tools::{Describe, Error, Result};
use tokio::process::Command;
use tonic::transport::Channel;

const EMULATOR: &str = "/opt/bigtable/cbtemulator";
/// What the emulator prints once it listens.
const EMULATOR_READY: &str = "Cloud Bigtable emulator running";
const STORAGE_SERVICE_MAIN: &str = "org.signal.storageservice.StorageService";
const STORAGE_SERVICE_CLASSPATH: &str = "/opt/storage-service/classes:/opt/storage-service/lib/*";
const PATIENCE: Duration = Duration::from_secs(300);

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let outcome = match testbox_tools::argument("storage") {
        Ok(None) => run().await,
        Ok(Some(other)) => Err(Error::Failed(format!("usage: storage, not {other}"))),
        Err(error) => Err(error),
    };
    testbox_tools::exit("storage", outcome)
}

async fn run() -> Result<()> {
    let (mut emulator, heard) =
        spawn_logged(Command::new(EMULATOR).args(["-address", BIGTABLE.address]))?;
    tokio::select! {
        ready = wait_for(heard, &[EMULATOR_READY], PATIENCE) => ready?,
        supervised = supervise(&mut emulator, "the emulator") => return supervised,
    }
    create_tables().await?;

    let (mut storage_service, heard) = spawn_logged(
        Command::new("java")
            .args(["-cp", STORAGE_SERVICE_CLASSPATH, STORAGE_SERVICE_MAIN])
            .arg("server")
            .arg(KeyFile::StorageService.path())
            // google-cloud-bigtable goes to the emulator when this is set
            // (BigtableDataSettings.newBuilder, 2.85.0).
            .env("BIGTABLE_EMULATOR_HOST", BIGTABLE.address),
    )?;
    tokio::select! {
        started = wait_for(heard, &[testbox_tools::JETTY_STARTED], PATIENCE) => started?,
        supervised = supervise(&mut emulator, "the emulator") => return supervised,
        supervised = supervise(&mut storage_service, "storage-service") => return supervised,
    }
    eprintln!("{}", testbox_tools::STORAGE_READY);
    // Until either stops, or SIGTERM stops both.
    tokio::select! {
        supervised = supervise(&mut emulator, "the emulator") => supervised,
        supervised = supervise(&mut storage_service, "storage-service") => supervised,
    }
}

async fn create_tables() -> Result<()> {
    let channel = Channel::from_shared(format!("http://{}", BIGTABLE.address))
        .describe("the emulator's address")?
        .connect()
        .await
        .describe("the emulator's admin API")?;
    let mut admin = BigtableTableAdminClient::new(channel);
    let instance = format!(
        "projects/{}/instances/{}",
        BIGTABLE.project, BIGTABLE.instance
    );
    for table in BIGTABLE.tables() {
        let request = CreateTableRequest {
            parent: instance.clone(),
            table_id: table.name.to_owned(),
            table: Some(Table {
                column_families: HashMap::from([(
                    table.family.to_owned(),
                    ColumnFamily::default(),
                )]),
                ..Table::default()
            }),
            ..CreateTableRequest::default()
        };
        admin
            .create_table(request)
            .await
            .describe(format!("creating table {}", table.name))?;
    }
    Ok(())
}
