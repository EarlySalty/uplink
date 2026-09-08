//! Root-interne Unix-Bridge. Keine Tokens, keine neuen TCP-Listener.
use nix::{
    fcntl::OFlag,
    sched::{CloneFlags, setns},
    sys::socket::{getsockopt, sockopt::PeerCredentials},
    unistd::geteuid,
};
use serde::Deserialize;
use std::{
    fs::{File, OpenOptions},
    io::Read,
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const MAX_CONNECTIONS: usize = 16;
const REFRESH: Duration = Duration::from_secs(2);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    socket_path: PathBuf,
    container_name: String,
    allowed_client_uids: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    container: String,
    pid: u32,
    path: PathBuf,
}

fn parse_identity(text: &str) -> Result<Identity, &'static str> {
    let fields: Vec<_> = text.split_whitespace().collect();
    if fields.len() != 4
        || fields[0].len() != 64
        || !fields[0].bytes().all(|byte| byte.is_ascii_hexdigit())
        || fields[1] != "true"
    {
        return Err("Infisical-Container ist nicht bestätigt aktiv.");
    }
    let pid = fields[2]
        .parse::<u32>()
        .ok()
        .filter(|pid| *pid > 1)
        .ok_or("Infisical-Containerprozess ist ungültig.")?;
    let suffix = fields[3]
        .strip_prefix("/var/run/docker/netns/")
        .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or("Docker-Netznamespace ist ungültig.")?;
    Ok(Identity {
        container: fields[0].to_owned(),
        pid,
        path: Path::new("/run/docker/netns").join(suffix),
    })
}

fn discover(name: &str) -> Result<Identity, &'static str> {
    // CLI liefert ausschließlich die vier öffentlichen Metadatenfelder. Weder
    // Container-ENV noch Tokens werden ausgelesen. Expliziter rootkontrollierter
    // Docker-Socket, keine geerbte DOCKER_HOST-/Kontext-/Plugin-Konfiguration.
    let mut child = Command::new("/usr/bin/docker")
        .env_clear()
        .args([
            "--config",
            "/run/uplink-infisical/docker-cli",
            "--host",
            "unix:///run/docker.sock",
            "inspect",
            "--type",
            "container",
            "--format",
            "{{.Id}} {{.State.Running}} {{.State.Pid}} {{.NetworkSettings.SandboxKey}}",
            name,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "Docker-Metadaten sind nicht verfügbar.")?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < Duration::from_secs(3) => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Docker-Metadaten wurden nicht rechtzeitig bestätigt.");
            }
        }
    };
    if !status.success() {
        return Err("Infisical-Container ist nicht verfügbar.");
    }
    let mut output = String::new();
    child
        .stdout
        .take()
        .ok_or("Docker-Metadaten fehlen.")?
        .take(513)
        .read_to_string(&mut output)
        .map_err(|_| "Docker-Metadaten sind ungültig.")?;
    if output.len() > 512 {
        return Err("Docker-Metadaten überschreiten die Grenze.");
    }
    parse_identity(&output)
}

fn namespace(identity: &Identity) -> Result<File, &'static str> {
    // Der offene FD hält exakt diese Namespace fest, auch nach Docker-Neustart.
    for path in [Path::new("/run/docker"), Path::new("/run/docker/netns")] {
        let metadata = std::fs::symlink_metadata(path).map_err(|_| "Docker-Pfad fehlt.")?;
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err("Docker-Pfad ist nicht rootgeschützt.");
        }
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(OFlag::O_NOFOLLOW.bits() | OFlag::O_CLOEXEC.bits())
        .open(&identity.path)
        .map_err(|_| "Docker-Netznamespace fehlt.")?;
    let actual = file
        .metadata()
        .map_err(|_| "Docker-Netznamespace ist ungültig.")?;
    let process = std::fs::metadata(format!("/proc/{}/ns/net", identity.pid))
        .map_err(|_| "Infisical-Containerprozess ist nicht verfügbar.")?;
    if actual.dev() != process.dev() || actual.ino() != process.ino() {
        return Err("Docker-Netznamespace und Containerprozess stimmen nicht überein.");
    }
    Ok(file)
}

fn serve(stream: UnixStream, namespace: Arc<File>, active: Arc<AtomicUsize>) {
    struct Reservation(Arc<AtomicUsize>);
    impl Drop for Reservation {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::Release);
        }
    }
    let _reservation = Reservation(active);
    // Ausschließlich dieser neue Workerthread wechselt die Netznamespace. Die
    // Supervisor-Verbindung zu Docker bleibt in der Hostnamespace.
    if setns(namespace.as_ref(), CloneFlags::CLONE_NEWNET).is_err() {
        return;
    }
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    let _ = stream.set_nonblocking(true);
    runtime.block_on(async {
        let Ok(mut client) = tokio::net::UnixStream::from_std(stream) else {
            return;
        };
        let Ok(Ok(mut backend)) = tokio::time::timeout(
            Duration::from_secs(3),
            tokio::net::TcpStream::connect("127.0.0.1:8080"),
        )
        .await
        else {
            return;
        };
        // Requests sind begrenzt; eine Verbindung darf Namespacewechsel und
        // Ressourcen nicht dauerhaft festhalten. Beide Richtungen enden zusammen.
        let _ = tokio::time::timeout(
            Duration::from_secs(15),
            tokio::io::copy_bidirectional(&mut client, &mut backend),
        )
        .await;
    });
}

fn accept_client(
    stream: UnixStream,
    namespace: Option<&Arc<File>>,
    allowed_uids: &[u32],
    active: &Arc<AtomicUsize>,
) -> Option<std::thread::JoinHandle<()>> {
    let allowed =
        getsockopt(&stream, PeerCredentials).is_ok_and(|peer| allowed_uids.contains(&peer.uid()));
    if let Some(namespace) = namespace
        && allowed
        && active.load(Ordering::Acquire) < MAX_CONNECTIONS
    {
        active.fetch_add(1, Ordering::AcqRel);
        let namespace = namespace.clone();
        let active = active.clone();
        Some(std::thread::spawn(move || serve(stream, namespace, active)))
    } else {
        None
    }
}

fn run() -> Result<(), &'static str> {
    if !geteuid().is_root() {
        return Err("Infisical-Bridge benötigt die vorgesehene Root-Unit.");
    }
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let path = match args.as_slice() {
        [flag, path] if flag == "--config" => PathBuf::from(path),
        _ => return Err("Aufruf: uplink-infisical-bridge --config <Datei>"),
    };
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(OFlag::O_NOFOLLOW.bits())
        .open(path)
        .map_err(|_| "Bridge-Konfiguration fehlt.")?;
    let metadata = file
        .metadata()
        .map_err(|_| "Bridge-Konfiguration ist ungültig.")?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || metadata.len() > 8192
    {
        return Err("Bridge-Konfiguration ist nicht rootgeschützt.");
    }
    let mut text = String::new();
    file.take(8193)
        .read_to_string(&mut text)
        .map_err(|_| "Bridge-Konfiguration ist unlesbar.")?;
    let config: Config = toml::from_str(&text).map_err(|_| "Bridge-Konfiguration ist ungültig.")?;
    if config.socket_path != Path::new(uplink_infisical_transport::DEFAULT_SOCKET)
        || config.container_name.is_empty()
        || config.container_name.len() > 128
        || !config
            .container_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
        || config.allowed_client_uids.is_empty()
        || config.allowed_client_uids.len() > 8
    {
        return Err("Bridge-Ziel oder Clientfreigabe ist ungültig.");
    }
    let directory = config
        .socket_path
        .parent()
        .ok_or("Socketverzeichnis fehlt.")?;
    let metadata = std::fs::symlink_metadata(directory).map_err(|_| "Socketverzeichnis fehlt.")?;
    if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err("Socketverzeichnis ist nicht rootgeschützt.");
    }
    let docker_config = directory.join("docker-cli");
    match std::fs::create_dir(&docker_config) {
        Ok(()) => std::fs::set_permissions(&docker_config, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| "Docker-Clientverzeichnis konnte nicht geschützt werden.")?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err("Docker-Clientverzeichnis fehlt."),
    }
    let metadata =
        std::fs::symlink_metadata(&docker_config).map_err(|_| "Docker-Clientverzeichnis fehlt.")?;
    if !metadata.is_dir()
        || metadata.uid() != 0
        || metadata.mode() & 0o077 != 0
        || std::fs::read_dir(&docker_config)
            .map_err(|_| "Docker-Clientverzeichnis ist unlesbar.")?
            .next()
            .is_some()
    {
        return Err("Docker-Clientverzeichnis muss privat und leer sein.");
    }
    // Normalerweise entfernt systemd RuntimeDirectory beim Stop. Nur einen
    // bestätigten eigenen alten Socket entfernen, nie Symlinks/Fremddateien.
    if config.socket_path.exists() {
        uplink_infisical_transport::validate_socket(&config.socket_path, 0)?;
        std::fs::remove_file(&config.socket_path).map_err(|_| "Alter Bridge-Socket ist belegt.")?;
    }
    let listener = UnixListener::bind(&config.socket_path)
        .map_err(|_| "Bridge-Socket konnte nicht gebunden werden.")?;
    std::fs::set_permissions(&config.socket_path, std::fs::Permissions::from_mode(0o660))
        .map_err(|_| "Bridge-Socketrechte konnten nicht gesetzt werden.")?;
    // Keine gemeinsame Gruppe mit fremden Diensten: exakt dieselbe UID-Liste
    // erhält POSIX-ACL-Zugriff und wird am verbundenen Socket erneut geprüft.
    let mut acl = String::from("user::rw-,group::---,mask::rw-,other::---");
    for uid in &config.allowed_client_uids {
        if *uid != 0 {
            acl.push_str(&format!(",user:{uid}:rw-"));
        }
    }
    let mut child = Command::new("/usr/bin/setfacl")
        .env_clear()
        .arg("--set")
        .arg(acl)
        .arg(&config.socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "Bridge-Socketfreigabe ist nicht verfügbar.")?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(None) if started.elapsed() < Duration::from_secs(2) => {
                std::thread::sleep(Duration::from_millis(10))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Bridge-Socketfreigabe wurde nicht bestätigt.");
            }
        }
    }
    listener
        .set_nonblocking(true)
        .map_err(|_| "Bridge-Socket ist nicht verfügbar.")?;
    let mut current: Option<(Identity, Arc<File>)> = None;
    let mut refresh_at = Instant::now();
    let mut failed = false;
    let active = Arc::new(AtomicUsize::new(0));
    loop {
        if Instant::now() >= refresh_at {
            let result = discover(&config.container_name).and_then(|identity| {
                let file = namespace(&identity)?;
                if discover(&config.container_name)? != identity {
                    return Err("Infisical-Container wurde während der Prüfung gewechselt.");
                }
                Ok((identity, Arc::new(file)))
            });
            match result {
                Ok(next) => {
                    if current.as_ref().map(|(id, _)| id) != Some(&next.0) {
                        eprintln!(
                            "Infisical-Netznamespace bestätigt; neue Verbindungen sind freigegeben."
                        );
                    }
                    current = Some(next);
                    failed = false;
                }
                Err(error) => {
                    current = None;
                    if !failed {
                        eprintln!("Infisical-Bridge wartet auf bestätigte Gegenstelle: {error}");
                    }
                    failed = true;
                }
            }
            refresh_at = Instant::now() + REFRESH;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                accept_client(
                    stream,
                    current.as_ref().map(|(_, namespace)| namespace),
                    &config.allowed_client_uids,
                    &active,
                );
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return Err("Bridge-Socket ist ausgefallen."),
        }
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "Liest als root ausschließlich bestätigte lokale Docker-Metadaten"]
    fn real_docker_namespace_metadata_is_consistent_read_only() {
        assert!(geteuid().is_root());
        let first = discover("infisical-backend").unwrap();
        namespace(&first).unwrap();
        assert_eq!(discover("infisical-backend").unwrap(), first);
    }
    /// Root in einer eigenen äußeren Netznamespace; keine Produktionslistener.
    #[test]
    #[ignore = "Benötigt Root-CAP_SYS_ADMIN und eine isolierte äußere Netznamespace"]
    fn real_namespaces_switch_without_host_port_or_unauthorized_peer() {
        use std::io::Write;
        use std::net::{Shutdown, TcpListener};
        assert!(geteuid().is_root());
        assert!(
            Command::new("/usr/sbin/ip")
                .env_clear()
                .args(["link", "set", "lo", "up"])
                .status()
                .unwrap()
                .success()
        );
        let hostile = TcpListener::bind("127.0.0.1:8080").unwrap();
        hostile.set_nonblocking(true).unwrap();
        let spawn_namespace = |reply: &'static [u8]| {
            let (send, receive) = std::sync::mpsc::channel();
            let worker = std::thread::spawn(move || {
                nix::sched::unshare(CloneFlags::CLONE_NEWNET).unwrap();
                assert!(
                    Command::new("/usr/sbin/ip")
                        .env_clear()
                        .args(["link", "set", "lo", "up"])
                        .status()
                        .unwrap()
                        .success()
                );
                let listener = TcpListener::bind("127.0.0.1:8080").unwrap();
                let namespace = Arc::new(
                    File::open(format!("/proc/self/task/{}/ns/net", nix::unistd::gettid()))
                        .unwrap(),
                );
                send.send(namespace).unwrap();
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut input = Vec::new();
                stream.read_to_end(&mut input).unwrap();
                assert_eq!(input, b"synthetic-identity-and-key");
                stream.write_all(reply).unwrap();
            });
            (
                receive.recv_timeout(Duration::from_secs(5)).unwrap(),
                worker,
            )
        };
        let active = Arc::new(AtomicUsize::new(0));
        let (first, first_worker) = spawn_namespace(b"namespace-one");
        let (second, second_worker) = spawn_namespace(b"namespace-two");
        assert_ne!(
            first.metadata().unwrap().ino(),
            second.metadata().unwrap().ino()
        );
        let request = |namespace: Option<&Arc<File>>, allowed: &[u32]| {
            let (mut client, server) = UnixStream::pair().unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let worker = accept_client(server, namespace, allowed, &active);
            let _ = client.write_all(b"synthetic-identity-and-key");
            let _ = client.shutdown(Shutdown::Write);
            let mut reply = Vec::new();
            let _ = client.read_to_end(&mut reply);
            if let Some(worker) = worker {
                worker.join().unwrap();
            }
            reply
        };
        assert!(
            request(Some(&first), &[1000]).is_empty(),
            "SO_PEERCRED muss Root ohne explizite Freigabe ablehnen"
        );
        assert_eq!(request(Some(&first), &[0]), b"namespace-one");
        first_worker.join().unwrap();
        assert!(
            request(None, &[0]).is_empty(),
            "Fehlende Gegenstelle darf keinen Hostport-Fallback auslösen"
        );
        assert_eq!(request(Some(&second), &[0]), b"namespace-two");
        second_worker.join().unwrap();
        assert_eq!(active.load(Ordering::Acquire), 0);
        assert_eq!(
            hostile.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
    #[test]
    fn namespace_metadata_requires_live_exact_container_and_canonical_docker_path() {
        let id = "a".repeat(64);
        assert!(parse_identity(&format!("{id} true 123 /var/run/docker/netns/012ab")).is_ok());
        for record in [
            format!("{id} false 123 /var/run/docker/netns/012ab"),
            format!("{id} true 0 /var/run/docker/netns/012ab"),
            format!("{id} true 123 /tmp/012ab"),
            format!("{id} true 123 /var/run/docker/netns/../012ab"),
            format!("{id} true 123 /var/run/docker/netns/012ab extra"),
        ] {
            assert!(parse_identity(&record).is_err());
        }
    }
}
