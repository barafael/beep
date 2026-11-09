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
use tokio::process::Command;
use tonic::transport::Channel;

const EMULATOR: &str = "/opt/bigtable/cbtemulator";
/// What the emulator prints once it listens.
const EMULATOR_READY: &str = "Cloud Bigtable emulator running";
const STORAGE_SERVICE_MAIN: &str = "org.signal.storageservice.StorageService";
const STORAGE_SERVICE_CLASSPATH: &str = "/opt/storage-service/classes:/opt/storage-service/lib/*";
const PATIENCE: Duration = Duration::from_secs(300);

/// What keeps the storage service from starting.
#[derive(Debug, thiserror::Error)]
enum StorageError {
    #[error("usage: storage, not {argument}")]
    Argument { argument: String },
    #[error("the emulator's address {address:?}")]
    Address {
        address: String,
        #[source]
        source: tonic::codegen::http::uri::InvalidUri,
    },
    #[error("connecting to the emulator's admin API at {address}")]
    Connect {
        address: String,
        #[source]
        source: tonic::transport::Error,
    },
    #[error("creating the table {table}")]
    CreateTable {
        table: &'static str,
        #[source]
        source: tonic::Status,
    },
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let outcome = match testbox_tools::argument("storage") {
        Ok(None) => run().await,
        Ok(Some(argument)) => Err(StorageError::Argument { argument }.into()),
        Err(error) => Err(error.into()),
    };
    testbox_tools::exit("storage", outcome)
}

async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (mut emulator, heard) =
        spawn_logged(Command::new(EMULATOR).args(["-address", BIGTABLE.address]))?;
    tokio::select! {
        ready = wait_for(heard, &[EMULATOR_READY], PATIENCE) => ready?,
        supervised = supervise(&mut emulator, "the emulator") => return Ok(supervised?),
    }
    create_tables().await?;

    let (mut storage_service, heard) = spawn_logged(
        Command::new("java")
            .args(testbox_tools::process::java_key_log_arguments())
            .args(["-cp", STORAGE_SERVICE_CLASSPATH, STORAGE_SERVICE_MAIN])
            .arg("server")
            .arg(KeyFile::StorageService.path())
            // google-cloud-bigtable goes to the emulator when this is set
            // (BigtableDataSettings.newBuilder, 2.85.0).
            .env("BIGTABLE_EMULATOR_HOST", BIGTABLE.address),
    )?;
    tokio::select! {
        started = wait_for(heard, &[testbox_tools::JETTY_STARTED], PATIENCE) => started?,
        supervised = supervise(&mut emulator, "the emulator") => return Ok(supervised?),
        supervised = supervise(&mut storage_service, "storage-service") => return Ok(supervised?),
    }
    eprintln!("{}", testbox_tools::STORAGE_READY);
    // Until either stops, or SIGTERM stops both.
    tokio::select! {
        supervised = supervise(&mut emulator, "the emulator") => Ok(supervised?),
        supervised = supervise(&mut storage_service, "storage-service") => Ok(supervised?),
    }
}

async fn create_tables() -> Result<(), StorageError> {
    let address = format!("http://{}", BIGTABLE.address);
    let channel = Channel::from_shared(address.clone())
        .map_err(|source| StorageError::Address {
            address: address.clone(),
            source,
        })?
        .connect()
        .await
        .map_err(|source| StorageError::Connect { address, source })?;
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
            .map_err(|source| StorageError::CreateTable {
                table: table.name,
                source,
            })?;
    }
    Ok(())
}
