use crate::{MediaError, Result};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    net::{UnixListener, UnixStream},
    time::timeout,
};

pub(crate) struct SocketDirectory {
    path: PathBuf,
    sockets: Vec<PathBuf>,
    closed: bool,
}
impl SocketDirectory {
    pub(crate) fn create(base: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(base).map_err(|_| MediaError::InvalidConfiguration)?;
        if !metadata.is_dir() || metadata.mode() & 0o077 != 0 {
            return Err(MediaError::InvalidConfiguration);
        }
        for _ in 0..16 {
            let mut random = [0; 16];
            getrandom::fill(&mut random).map_err(|_| MediaError::Io)?;
            let name = random
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            let path = base.join(format!("media-{name}"));
            if path.as_os_str().len() > 80 {
                return Err(MediaError::InvalidConfiguration);
            }
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => {
                    return Ok(Self {
                        path,
                        sockets: Vec::new(),
                        closed: false,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(MediaError::Io),
            }
        }
        Err(MediaError::Io)
    }
    pub(crate) fn close(mut self) -> Result<()> {
        self.closed = true;
        self.remove_owned()
    }
    fn remove_owned(&self) -> Result<()> {
        let mut failed = false;
        for path in &self.sockets {
            if fs::remove_file(path)
                .is_err_and(|error| error.kind() != std::io::ErrorKind::NotFound)
            {
                failed = true;
            }
        }
        if fs::remove_dir(&self.path)
            .is_err_and(|error| error.kind() != std::io::ErrorKind::NotFound)
        {
            failed = true;
        }
        if failed {
            Err(MediaError::ProcessCleanupFailed)
        } else {
            Ok(())
        }
    }
    pub(crate) fn listener(&mut self, index: usize) -> Result<(UnixListener, PathBuf)> {
        if self.sockets.len() >= 32 {
            return Err(MediaError::ResourceLimit);
        }
        let path = self.path.join(format!("{index}.sock"));
        let listener = UnixListener::bind(&path).map_err(|_| MediaError::Io)?;
        self.sockets.push(path.clone());
        Ok((listener, path))
    }
}
impl Drop for SocketDirectory {
    fn drop(&mut self) {
        // Ausschließlich die selbst angelegten Sockets und der eigene leere Ordner.
        if !self.closed && self.remove_owned().is_err() {
            eprintln!("Private Medien-Sockets konnten nicht vollständig entfernt werden.");
        }
    }
}
pub(crate) async fn accept_worker(
    listener: &UnixListener,
    pid: u32,
    deadline: Duration,
) -> Result<UnixStream> {
    let (stream, _) = timeout(deadline, listener.accept())
        .await
        .map_err(|_| MediaError::StartTimeout)?
        .map_err(|_| MediaError::Io)?;
    if stream
        .peer_cred()
        .map_err(|_| MediaError::Io)?
        .pid()
        .and_then(|value| u32::try_from(value).ok())
        != Some(pid)
    {
        return Err(MediaError::WrongSession);
    }
    Ok(stream)
}
