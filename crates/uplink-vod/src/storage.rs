use crate::{Error, Result, store::validate_object_id};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::io::AsyncReadExt;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportFile {
    pub bytes: u64,
    pub sha256: String,
}
#[derive(Clone)]
pub struct Storage {
    root: Arc<PathBuf>,
    _root_lock: Arc<std::fs::File>,
    used: Arc<AtomicU64>,
    limit: u64,
}
impl Storage {
    pub fn new(root: PathBuf, limit: u64) -> Result<Self> {
        if !root.is_absolute() || limit == 0 {
            return Err(Error::Invalid);
        }
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
        if !root.exists() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&root)
                .map_err(|_| Error::Storage)?
        }
        let metadata = std::fs::symlink_metadata(&root).map_err(|_| Error::Storage)?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(Error::Storage);
        }
        let root = root.canonicalize().map_err(|_| Error::Storage)?;
        // Alle Worker desselben Speichers teilen einen geklonten Adapter.
        // Zweite Prozesse dürfen dasselbe freie Bytebudget nicht erneut vergeben.
        let root_lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(root.join(".storage.lock"))
            .map_err(|_| Error::Storage)?;
        root_lock.try_lock().map_err(|_| Error::Storage)?;
        let mut bytes = 0u64;
        for entry in std::fs::read_dir(&root).map_err(|_| Error::Storage)? {
            let entry = entry.map_err(|_| Error::Storage)?;
            if entry.file_name() == ".storage.lock" {
                continue;
            }
            if !entry.file_type().map_err(|_| Error::Storage)?.is_dir() {
                return Err(Error::Storage);
            }
            validate_object_id(&entry.file_name().to_string_lossy())?;
            for file in std::fs::read_dir(entry.path()).map_err(|_| Error::Storage)? {
                let file = file.map_err(|_| Error::Storage)?;
                let meta = file.metadata().map_err(|_| Error::Storage)?;
                if file.file_type().map_err(|_| Error::Storage)?.is_symlink() || !meta.is_file() {
                    return Err(Error::Storage);
                }
                bytes = bytes.checked_add(meta.len()).ok_or(Error::StorageFull)?;
            }
        }
        Ok(Self {
            root: Arc::new(root),
            _root_lock: Arc::new(root_lock),
            used: Arc::new(AtomicU64::new(bytes)),
            limit,
        })
    }
    pub fn used_bytes(&self) -> u64 {
        self.used.load(Ordering::Acquire)
    }
    pub fn available_bytes(&self) -> u64 {
        self.limit.saturating_sub(self.used_bytes())
    }
    pub fn reserve(&self, bytes: u64) -> Result<Reservation> {
        self.used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes).filter(|sum| *sum <= self.limit)
            })
            .map_err(|_| Error::StorageFull)?;
        Ok(Reservation {
            used: self.used.clone(),
            bytes,
            committed: false,
        })
    }
    pub fn create_object(&self) -> Result<String> {
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|_| Error::Storage)?;
        let id = hex::encode(random);
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(self.root.join(&id))
            .map_err(|_| Error::Storage)?;
        Ok(id)
    }
    pub fn object_dir(&self, id: &str) -> Result<PathBuf> {
        validate_object_id(id)?;
        let path = self.root.join(id);
        let meta = std::fs::symlink_metadata(&path).map_err(|_| Error::Storage)?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err(Error::Storage);
        }
        Ok(path)
    }
    pub fn path(&self, id: &str, name: &str) -> Result<PathBuf> {
        if name.is_empty()
            || name.len() > 80
            || name.starts_with('.')
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
        {
            return Err(Error::Storage);
        }
        Ok(self.object_dir(id)?.join(name))
    }
    pub fn lock_preparation(&self, id: &str) -> Result<std::fs::File> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.path(id, "prepare.lock")?)
            .map_err(|_| Error::Storage)?;
        file.try_lock().map_err(|_| Error::SourcePending)?;
        Ok(file)
    }
    pub async fn write_manifest(&self, id: &str, value: &impl Serialize) -> Result<()> {
        let bytes = serde_json::to_vec(value).map_err(|_| Error::Storage)?;
        if bytes.len() > 1024 * 1024 {
            return Err(Error::StorageFull);
        }
        let path = self.path(id, "recording.json")?;
        let previous = match tokio::fs::symlink_metadata(&path).await {
            Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => meta.len(),
            Ok(_) => return Err(Error::Storage),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(_) => return Err(Error::Storage),
        };
        self.reserve(bytes.len() as u64)?.commit();
        atomic_json(&path, value).await?;
        let _ = self
            .used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                Some(used.saturating_sub(previous))
            });
        Ok(())
    }
    pub async fn retire_download(&self, id: &str) -> Result<()> {
        self.retire_file(id, "export.mp4").await?;
        self.retire_file(id, "download.ts").await
    }
    async fn retire_file(&self, id: &str, filename: &str) -> Result<()> {
        let old = self.path(id, filename)?;
        if !old.exists() {
            return Ok(());
        }
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).map_err(|_| Error::Storage)?;
        let name = format!("{filename}-outdated-{}.part", hex::encode(random));
        tokio::fs::rename(old, self.path(id, &name)?)
            .await
            .map_err(|_| Error::Storage)
    }
    pub async fn open(&self, id: &str, name: &str) -> Result<tokio::fs::File> {
        let path = self.path(id, name)?;
        let meta = tokio::fs::symlink_metadata(&path)
            .await
            .map_err(|_| Error::Storage)?;
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err(Error::Storage);
        }
        tokio::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .await
            .map_err(|_| Error::Storage)
    }
    pub async fn digest(&self, id: &str, name: &str) -> Result<ExportFile> {
        let mut file = self.open(id, name).await?;
        let mut hash = Sha256::new();
        let mut total = 0;
        let mut buffer = vec![0; 128 * 1024];
        loop {
            let len = file.read(&mut buffer).await.map_err(|_| Error::Storage)?;
            if len == 0 {
                break;
            }
            hash.update(&buffer[..len]);
            total += len as u64;
        }
        if total == 0 {
            return Err(Error::Incomplete);
        }
        Ok(ExportFile {
            bytes: total,
            sha256: hex::encode(hash.finalize()),
        })
    }
    /// Nur nach dauerhafter Löschfreigabe des Stores aufrufen. Kein Twitch-Plattform-VOD löschen.
    pub async fn delete_object(&self, id: &str) -> Result<()> {
        validate_object_id(id)?;
        let raw = self.root.join(id);
        if matches!(tokio::fs::symlink_metadata(&raw).await,Err(ref e) if e.kind()==std::io::ErrorKind::NotFound)
        {
            return Ok(());
        }
        let path = self.object_dir(id)?;
        let mut entries = tokio::fs::read_dir(&path)
            .await
            .map_err(|_| Error::Storage)?;
        while let Some(entry) = entries.next_entry().await.map_err(|_| Error::Storage)? {
            let meta = entry.metadata().await.map_err(|_| Error::Storage)?;
            if !meta.is_file()
                || entry
                    .file_type()
                    .await
                    .map_err(|_| Error::Storage)?
                    .is_symlink()
            {
                return Err(Error::Storage);
            }
            tokio::fs::remove_file(entry.path())
                .await
                .map_err(|_| Error::Storage)?;
            let _ = self
                .used
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |v| {
                    Some(v.saturating_sub(meta.len()))
                });
        }
        tokio::fs::remove_dir(path)
            .await
            .map_err(|_| Error::Storage)?;
        Ok(())
    }
    pub(crate) fn contains(&self, id: &str, name: &str) -> bool {
        self.path(id, name).is_ok_and(|p| p.exists())
    }
}
pub struct Reservation {
    used: Arc<AtomicU64>,
    bytes: u64,
    committed: bool,
}
impl Reservation {
    pub fn commit(mut self) {
        self.committed = true
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.committed {
            self.used.fetch_sub(self.bytes, Ordering::AcqRel);
        }
    }
}
pub(crate) async fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|_| Error::Storage)?;
    if bytes.len() > 1024 * 1024 {
        return Err(Error::StorageFull);
    }
    let temp = path.with_extension("json-new");
    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temp)
        .await
        .map_err(|_| Error::Storage)?;
    file.write_all(&bytes).await.map_err(|_| Error::Storage)?;
    file.sync_all().await.map_err(|_| Error::Storage)?;
    drop(file);
    tokio::fs::rename(temp, path)
        .await
        .map_err(|_| Error::Storage)?;
    let parent = tokio::fs::File::open(path.parent().ok_or(Error::Storage)?)
        .await
        .map_err(|_| Error::Storage)?;
    parent.sync_all().await.map_err(|_| Error::Storage)?;
    Ok(())
}
