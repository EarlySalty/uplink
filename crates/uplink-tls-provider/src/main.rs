use std::{path::PathBuf, process::ExitCode};
use uplink_tls_provider::{Error, SyncStatus};

async fn run() -> Result<SyncStatus, Error> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--config")) {
        return Err(Error::Configuration);
    }
    let path = PathBuf::from(args.next().ok_or(Error::Configuration)?);
    if args.next().is_some() {
        return Err(Error::Configuration);
    }
    let config = uplink_tls_provider::load_config(&path).await?;
    uplink_tls_provider::synchronize(&config).await
}
#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(SyncStatus::Unchanged) => ExitCode::SUCCESS,
        Ok(SyncStatus::Updated) => {
            println!("Uplink-TLS-Paar aktualisiert und bestätigt.");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
