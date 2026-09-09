use nix::fcntl::OFlag;
use std::{
    fs::{DirBuilder, File, OpenOptions},
    io::Write,
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Component, Path, PathBuf},
};

pub(crate) const SLOT: &str = "obs-probe-dump";
pub(crate) const FILE: &str = "input.flv";
const PARTIAL: &str = ".partial";

pub(crate) fn spawn(
    reservation: std::sync::Arc<crate::registry::Reservation>,
    directory: PathBuf,
    bytes: Vec<u8>,
) -> tokio::task::JoinHandle<DumpStatus> {
    tokio::task::spawn_blocking(move || {
        let _reservation = reservation;
        write(&directory, &bytes)
    })
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DumpStatus {
    Saved,
    SlotOccupied,
    TooLarge,
    UnsafeDirectory,
    WriteFailed,
    CleanupFailed,
}
impl DumpStatus {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Saved => "saved",
            Self::SlotOccupied => "slot_occupied",
            Self::TooLarge => "too_large",
            Self::UnsafeDirectory => "unsafe_directory",
            Self::WriteFailed => "write_failed",
            Self::CleanupFailed => "partial_cleanup_failed",
        }
    }
}

fn fd_path(directory: &File) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()))
}
fn open_directory(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags((OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC).bits())
        .open(path)
}
fn anchored_directory(path: &Path) -> std::io::Result<File> {
    if !path.is_absolute() {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    let mut directory = open_directory(Path::new("/"))?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => directory = open_directory(&fd_path(&directory).join(name))?,
            _ => return Err(std::io::ErrorKind::InvalidInput.into()),
        }
    }
    if directory.metadata()?.mode() & 0o077 != 0 {
        return Err(std::io::ErrorKind::PermissionDenied.into());
    }
    Ok(directory)
}

struct Slot {
    parent: File,
    directory: File,
    keep: bool,
    linked: bool,
}
impl Slot {
    fn cleanup(&mut self) -> bool {
        let remove = |name: &str| match std::fs::remove_file(fd_path(&self.directory).join(name)) {
            Ok(()) => true,
            Err(error) => error.kind() == std::io::ErrorKind::NotFound,
        };
        let file_removed = !self.linked || remove(FILE);
        let partial_removed = remove(PARTIAL);
        let directory_removed = file_removed
            && partial_removed
            && std::fs::remove_dir(fd_path(&self.parent).join(SLOT)).is_ok();
        self.keep = true;
        directory_removed
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        if !self.keep {
            self.cleanup();
        }
    }
}

pub(crate) fn write(directory: &Path, bytes: &[u8]) -> DumpStatus {
    if bytes.len() > uplink_media::MAX_PROBE_DUMP_BYTES {
        return DumpStatus::TooLarge;
    }
    let parent = match anchored_directory(directory) {
        Ok(directory) => directory,
        Err(_) => return DumpStatus::UnsafeDirectory,
    };
    let slot_path = fd_path(&parent).join(SLOT);
    if let Err(error) = DirBuilder::new().mode(0o700).create(&slot_path) {
        return if error.kind() == std::io::ErrorKind::AlreadyExists {
            DumpStatus::SlotOccupied
        } else {
            DumpStatus::WriteFailed
        };
    }
    let directory = match open_directory(&slot_path) {
        Ok(directory) => directory,
        Err(_) => {
            return if std::fs::remove_dir(slot_path).is_ok() {
                DumpStatus::WriteFailed
            } else {
                DumpStatus::CleanupFailed
            };
        }
    };
    let mut slot = Slot {
        parent,
        directory,
        keep: false,
        linked: false,
    };
    let result = (|| -> std::io::Result<()> {
        let partial = fd_path(&slot.directory).join(PARTIAL);
        let complete = fd_path(&slot.directory).join(FILE);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags((OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC).bits())
            .open(&partial)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::hard_link(&partial, complete)?;
        slot.linked = true;
        std::fs::remove_file(partial)?;
        slot.directory.sync_all()?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            slot.keep = true;
            DumpStatus::Saved
        }
        Err(_) => {
            if slot.cleanup() {
                DumpStatus::WriteFailed
            } else {
                DumpStatus::CleanupFailed
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};

    fn directory() -> std::path::PathBuf {
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).unwrap();
        let path = std::path::PathBuf::from(format!(
            "/tmp/uplink-dump-test-{:016x}",
            u64::from_ne_bytes(random)
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();
        path
    }

    #[test]
    fn dump_is_complete_private_and_never_overwritten() {
        let directory = directory();
        let bytes = b"FLV\x01\x05\x00\x00\x00\x09\x00\x00\x00\x00synthetic-media";
        assert_eq!(write(&directory, bytes), DumpStatus::Saved);
        let path = directory.join(SLOT).join(FILE);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        assert_eq!(write(&directory, b"replacement"), DumpStatus::SlotOccupied);
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn queued_dump_keeps_admission_reserved_after_coordinator_cancellation() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let directory = directory();
            let registry = crate::registry::Registry::new(1, 1).unwrap();
            let reservation = std::sync::Arc::new(registry.reserve(538636411).unwrap());
            let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                ready_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
            ready_rx.await.unwrap();
            let (queued_tx, queued_rx) = tokio::sync::oneshot::channel();
            let destination = directory.clone();
            let coordinator = tokio::spawn(async move {
                let dump = spawn(reservation.clone(), destination, b"complete".to_vec());
                queued_tx.send(()).unwrap();
                dump.await.unwrap()
            });
            queued_rx.await.unwrap();
            coordinator.abort();
            assert!(coordinator.await.unwrap_err().is_cancelled());
            let active_while_queued = registry.active_count();
            let replacement_rejected = registry.reserve(538636411).is_err();
            release_tx.send(()).unwrap();
            blocker.await.unwrap();
            tokio::task::spawn_blocking(|| {}).await.unwrap();
            assert_eq!(
                std::fs::read(directory.join(SLOT).join(FILE)).unwrap(),
                b"complete"
            );
            std::fs::remove_dir_all(directory).unwrap();
            assert_eq!(active_while_queued, 1);
            assert!(replacement_rejected);
            assert_eq!(registry.active_count(), 0);
            assert!(registry.reserve(538636411).is_ok());
        });
    }

    #[test]
    fn dump_refuses_directory_accessible_to_other_users() {
        let directory = directory();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(write(&directory, b"private"), DumpStatus::UnsafeDirectory);
        assert!(!directory.join(SLOT).exists());
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn dump_size_boundary_preserves_whole_file_and_skips_oversize() {
        let directory = directory();
        let limit = uplink_media::MAX_PROBE_DUMP_BYTES;
        assert_eq!(write(&directory, &vec![7; limit + 1]), DumpStatus::TooLarge);
        assert!(!directory.join(SLOT).exists());
        let bytes = vec![9; limit];
        assert_eq!(write(&directory, &bytes), DumpStatus::Saved);
        assert_eq!(
            std::fs::read(directory.join(SLOT).join(FILE)).unwrap(),
            bytes
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn dump_refuses_symlinks_in_directory_or_slot() {
        use std::os::unix::fs::symlink;
        let directory = directory();
        let real = directory.join("real");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&real)
            .unwrap();
        let alias = directory.join("alias");
        symlink(&real, &alias).unwrap();
        assert_eq!(write(&alias, b"media"), DumpStatus::UnsafeDirectory);
        let nested = real.join("nested");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&nested)
            .unwrap();
        assert_eq!(
            write(&alias.join("nested"), b"media"),
            DumpStatus::UnsafeDirectory
        );
        symlink(&real, directory.join(SLOT)).unwrap();
        assert_eq!(write(&directory, b"media"), DumpStatus::SlotOccupied);
        assert!(
            std::fs::read_dir(&real)
                .unwrap()
                .all(|entry| entry.unwrap().file_name() == "nested")
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn concurrent_dumps_share_one_exclusive_slot() {
        let directory = directory();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let tasks: Vec<_> = (0..8u8)
            .map(|value| {
                let directory = directory.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    (value, write(&directory, &vec![value; 65536]))
                })
            })
            .collect();
        let results: Vec<_> = tasks.into_iter().map(|task| task.join().unwrap()).collect();
        let saved: Vec<_> = results
            .iter()
            .filter(|(_, status)| *status == DumpStatus::Saved)
            .collect();
        assert_eq!(saved.len(), 1);
        assert!(
            results
                .iter()
                .all(|(_, status)| matches!(status, DumpStatus::Saved | DumpStatus::SlotOccupied))
        );
        assert_eq!(
            std::fs::read(directory.join(SLOT).join(FILE)).unwrap(),
            vec![saved[0].0; 65536]
        );
        assert_eq!(std::fs::read_dir(directory.join(SLOT)).unwrap().count(), 1);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn abandoned_partial_is_removed_and_crash_links_share_one_inode() {
        let directory = directory();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(directory.join(SLOT))
            .unwrap();
        let mut slot = Slot {
            parent: anchored_directory(&directory).unwrap(),
            directory: open_directory(&directory.join(SLOT)).unwrap(),
            keep: false,
            linked: false,
        };
        std::fs::write(directory.join(SLOT).join(PARTIAL), b"incomplete").unwrap();
        assert!(slot.cleanup());
        drop(slot);
        assert!(!directory.join(SLOT).exists());
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(directory.join(SLOT))
            .unwrap();
        std::fs::write(directory.join(SLOT).join(PARTIAL), b"complete").unwrap();
        std::fs::hard_link(
            directory.join(SLOT).join(PARTIAL),
            directory.join(SLOT).join(FILE),
        )
        .unwrap();
        let partial = std::fs::metadata(directory.join(SLOT).join(PARTIAL)).unwrap();
        let complete = std::fs::metadata(directory.join(SLOT).join(FILE)).unwrap();
        assert_eq!(partial.ino(), complete.ino());
        assert_eq!(complete.nlink(), 2);
        assert_eq!(write(&directory, b"another"), DumpStatus::SlotOccupied);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
