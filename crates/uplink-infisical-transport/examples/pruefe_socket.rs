//! Prüft ausschließlich öffentliche Socketmetadaten, ohne HTTP oder Secrets.
use std::{os::unix::fs::MetadataExt, path::PathBuf, process::ExitCode};

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let [path, owner] = args.as_slice() else {
        eprintln!("Aufruf: pruefe_socket <absoluter Socketpfad> <Besitzer-UID>");
        return ExitCode::FAILURE;
    };
    let Some(owner) = owner.to_str().and_then(|value| value.parse::<u32>().ok()) else {
        eprintln!("Besitzer-UID ist ungültig.");
        return ExitCode::FAILURE;
    };
    let path = PathBuf::from(path);
    println!(
        "Prüfung als UID {}, erwartet UID {owner}",
        nix::unistd::geteuid()
    );
    for component in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
        match std::fs::symlink_metadata(component) {
            Ok(metadata) => println!(
                "{}: UID {}, GID {}, Modus {:o}",
                component.display(),
                metadata.uid(),
                metadata.gid(),
                metadata.mode() & 0o7777
            ),
            Err(error) => println!("{}: {error}", component.display()),
        }
    }
    match uplink_infisical_transport::client_builder(&path, owner) {
        Ok(_) => {
            println!(
                "Produktionsprüfung bestanden. Keine Verbindung und keine Secretabfrage ausgeführt."
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("Produktionsprüfung abgelehnt: {error}");
            ExitCode::FAILURE
        }
    }
}
