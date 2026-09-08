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
