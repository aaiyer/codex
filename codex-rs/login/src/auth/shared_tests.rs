use super::*;
use pretty_assertions::assert_eq;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;

#[test]
fn shared_storage_binds_private_directory_and_atomic_credentials() -> anyhow::Result<()> {
    let parent = tempfile::tempdir()?;
    let root = parent.path().join("auth");
    std::fs::create_dir(&root)?;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    let directory = std::fs::File::open(&root)?;
    let descriptor = PathBuf::from(format!(
        "/proc/{}/fd/{}",
        std::process::id(),
        directory.as_raw_fd()
    ));
    let storage = SharedAuthStorage {
        root: descriptor,
        lease: None,
    };
    let auth = read_auth_json(br#"{"OPENAI_API_KEY":"secret-a"}"#.as_slice())?;
    storage.save(&auth)?;
    let old_file = std::fs::File::open(root.join("auth.json"))?;
    let replacement = read_auth_json(br#"{"OPENAI_API_KEY":"secret-b"}"#.as_slice())?;
    std::fs::rename(&root, parent.path().join("renamed"))?;
    storage.save(&replacement)?;
    assert_eq!(storage.load()?, Some(replacement));
    assert_eq!(read_auth_json(old_file)?, auth);
    let transaction = storage.transaction()?.unwrap();
    let lock = std::fs::File::open(parent.path().join("renamed/auth.lock"))?;
    assert!(matches!(
        lock.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    drop(transaction);
    lock.try_lock()?;
    drop(lock);
    assert!(storage.delete()?);
    assert_eq!(storage.load()?, None);
    Ok(())
}

#[test]
fn shared_storage_and_import_reject_untrusted_authority_and_payloads() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755))?;
    let storage = SharedAuthStorage {
        root: root.path().to_path_buf(),
        lease: None,
    };
    assert!(storage.load().is_err());
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))?;
    let target = root.path().join("outside");
    std::fs::write(&target, "untouched")?;
    std::os::unix::fs::symlink(&target, root.path().join("auth.json"))?;
    assert!(storage.load().is_err());
    assert_eq!(std::fs::read_to_string(target)?, "untouched");
    for payload in [
        b"{}".as_slice(),
        b"{invalid}",
        br#"{"OPENAI_API_KEY":""}"#,
        br#"{"auth_mode":"chatgpt","tokens":null}"#,
    ] {
        assert!(read_auth_json(payload).is_err());
    }
    assert!(read_auth_json(std::io::repeat(b' ').take(MAX_AUTH_JSON_BYTES + 1)).is_err());
    for path in ["relative", "/proc/self/fd/0", "/proc/1/fd/nope"] {
        assert!(
            SharedAuthStorage {
                root: path.into(),
                lease: None
            }
            .load()
            .is_err()
        );
    }
    Ok(())
}
