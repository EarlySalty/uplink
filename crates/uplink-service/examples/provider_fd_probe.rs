//! Explicit Linux probe: requires passwordless sudo and setpriv; no skipped success.
//! Uses only three artificial values in temporary root-owned 0600 files.
use std::{os::unix::fs::MetadataExt, process::Command};
use uplink_service::{
    config::{Config, TlsConfig},
    secrets::{protect_configured_fds, read_fd},
};

#[tokio::main]
async fn main() {
    let result = match std::env::args().skip(1).collect::<Vec<_>>().as_slice() {
        [] => provider(),
        [flag] if flag == "--provider-child" => receiver().await,
        _ => Err("Aufruf: provider_fd_probe"),
    };
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
    println!("FD-Probe erfolgreich.");
}

fn provider() -> Result<(), &'static str> {
    let identity = std::fs::metadata("/proc/self").map_err(|_| "Prozessidentität fehlt.")?;
    if identity.uid() == 0 {
        return Err("Probe muss als unprivilegierter Dienstnutzer gestartet werden.");
    }
    let executable = std::env::current_exe().map_err(|_| "Probeprogramm fehlt.")?;
    // The privileged provider opens the synthetic files, then drops identity before
    // executing this same receiver. Only public fixture values ever reach these files.
    let status = Command::new("/usr/bin/sudo")
        .args([
            "-n",
            "/bin/sh",
            "-c",
            r#"
set -eu
fixture_dir=$(mktemp -d /tmp/uplink-provider-fd.XXXXXXXX)
trap 'rm -rf -- "$fixture_dir"' EXIT
umask 077
printf '%s' synthetic-bootstrap > "$fixture_dir/bootstrap"
printf '%s' synthetic-certificate > "$fixture_dir/certificate"
printf '%s' synthetic-private-key > "$fixture_dir/key"
exec 5< "$fixture_dir/bootstrap"
exec 6< "$fixture_dir/certificate"
exec 7< "$fixture_dir/key"
/usr/bin/setpriv --reuid "$1" --regid "$2" --clear-groups -- "$3" --provider-child
"#,
            "uplink-synthetic-provider",
        ])
        .arg(identity.uid().to_string())
        .arg(identity.gid().to_string())
        .arg(executable)
        .status()
        .map_err(|_| "Privilegierter Testprovider konnte nicht gestartet werden.")?;
    if !status.success() {
        return Err("FD-Probe fehlgeschlagen; sudo/setpriv und Empfänger prüfen.");
    }
    Ok(())
}

async fn receiver() -> Result<(), &'static str> {
    if std::fs::metadata("/proc/self")
        .map_err(|_| "Prozessidentität fehlt.")?
        .uid()
        == 0
    {
        return Err("Empfänger läuft noch privilegiert.");
    }
    let mut config = Config::parse(include_str!("../../../config/uplink-beispiel.toml"))?;
    config.infisical.credential_fd = 5;
    config.tls = TlsConfig::Fds {
        certificate_fd: 6,
        private_key_fd: 7,
    };
    protect_configured_fds(&config)?;
    for (fd, expected) in [
        (5, b"synthetic-bootstrap".as_slice()),
        (6, b"synthetic-certificate"),
        (7, b"synthetic-private-key"),
    ] {
        let path = format!("/proc/self/fd/{fd}");
        let metadata = std::fs::metadata(&path).map_err(|_| "Übergebener Test-FD fehlt.")?;
        if metadata.uid() != 0 || metadata.mode() & 0o777 != 0o600 {
            return Err("Providerdatei hat falsche Testrechte.");
        }
        if !matches!(std::fs::File::open(&path), Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied)
        {
            return Err("Pfad-Reopen wurde nicht durch Dateirechte abgewiesen.");
        }
        let value = read_fd(fd, 1024).await?;
        if !value.matches(expected) {
            return Err("Übergebene Testdaten sind verändert.");
        }
        let child = Command::new("/usr/bin/test")
            .args(["-e", &path])
            .status()
            .map_err(|_| "Kindprozess konnte nicht gestartet werden.")?;
        if child.success() {
            return Err("Original-FD wurde an Kindprozess vererbt.");
        }
    }
    Ok(())
}
