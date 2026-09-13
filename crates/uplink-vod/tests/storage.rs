use uplink_vod::{Error, Storage};

#[test]
fn aufbereitung_ist_exklusiv_und_abbruch_gibt_dateisperre_frei() {
    let directory = tempfile::tempdir().unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let storage = Storage::new(directory.path().to_owned(), 1024).unwrap();
    let object = storage.create_object().unwrap();
    let lease = storage.lock_preparation(&object).unwrap();
    assert_eq!(
        storage.lock_preparation(&object).unwrap_err(),
        Error::SourcePending
    );
    drop(lease);
    let next = storage.lock_preparation(&object).unwrap();
    drop(next);
}

#[test]
fn speicherbudget_hat_nur_einen_besitzer_und_klone_teilen_es() {
    let directory = tempfile::tempdir().unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let storage = Storage::new(directory.path().to_owned(), 1024).unwrap();
    assert!(matches!(
        Storage::new(directory.path().to_owned(), 1024),
        Err(Error::Storage)
    ));
    let clone = storage.clone();
    let reservation = clone.reserve(1024).unwrap();
    assert!(matches!(storage.reserve(1), Err(Error::StorageFull)));
    drop(reservation);
    drop(clone);
    drop(storage);
    assert!(Storage::new(directory.path().to_owned(), 1024).is_ok());
}

#[tokio::test]
async fn wiederanlauf_und_veraltete_zwischenprodukte_erhalten_die_eingangsquelle() {
    let directory = tempfile::tempdir().unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let storage = Storage::new(directory.path().to_owned(), 64).unwrap();
    let object = storage.create_object().unwrap();
    for name in [
        "input-0000.flv",
        "export.mp4",
        "download.ts",
        "download.ts-0123456789abcdef.part",
        "export.mp4-outdated-0123456789abcdef.part",
    ] {
        std::fs::write(storage.path(&object, name).unwrap(), b"media").unwrap();
    }
    drop(storage);
    let storage = Storage::new(directory.path().to_owned(), 64).unwrap();
    assert_eq!(storage.used_bytes(), 15);
    storage.retire_download(&object).await.unwrap();
    assert_eq!(storage.used_bytes(), 5);
    assert_eq!(
        tokio::fs::read(storage.path(&object, "input-0000.flv").unwrap())
            .await
            .unwrap(),
        b"media"
    );
}
