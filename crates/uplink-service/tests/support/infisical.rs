use std::{
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::PathBuf,
};

/// Ausschließlich synthetischer Infisical-Testserver, ohne TCP-Listener.
pub struct InfisicalMock {
    pub path: PathBuf,
    pub owner: u32,
    task: tokio::task::JoinHandle<()>,
    directory: PathBuf,
}
impl InfisicalMock {
    pub fn start(router: axum::Router) -> Self {
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).unwrap();
        let directory = PathBuf::from("/tmp").join(format!("ul-uds-{}", hex::encode(nonce)));
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.join("api.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let owner = std::fs::metadata(&path).unwrap().uid();
        let task = tokio::spawn(async { axum::serve(listener, router).await.unwrap() });
        Self {
            path,
            owner,
            task,
            directory,
        }
    }
}
impl Drop for InfisicalMock {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_dir(&self.directory);
    }
}
