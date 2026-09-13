//! Explicit shared file credentials. Ephemeral authentication never selects this store.
use super::storage::AuthDotJson;
use super::storage::AuthStorageBackend;
use codex_config::types::AuthCredentialsStoreMode;
use std::io;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

pub const MAX_AUTH_JSON_BYTES: u64 = 1024 * 1024;

pub fn shared_auth_enabled(mode: AuthCredentialsStoreMode) -> bool {
    mode != AuthCredentialsStoreMode::Ephemeral && std::env::var_os("CODEX_AUTH_HOME").is_some()
}

pub(super) fn selected_storage(
    mode: AuthCredentialsStoreMode,
) -> Option<Arc<dyn AuthStorageBackend>> {
    if mode == AuthCredentialsStoreMode::Ephemeral {
        return None;
    }
    std::env::var_os("CODEX_AUTH_HOME").map(|root| storage_at(PathBuf::from(root)))
}

fn storage_at(root: PathBuf) -> Arc<dyn AuthStorageBackend> {
    Arc::new(SharedAuthStorage {
        root,
        #[cfg(target_os = "linux")]
        lease: None,
    })
}

pub fn read_auth_json(reader: impl Read) -> io::Result<AuthDotJson> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_AUTH_JSON_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_AUTH_JSON_BYTES {
        return Err(io::Error::other("auth JSON exceeds 1 MiB"));
    }
    let auth: AuthDotJson =
        serde_json::from_slice(&bytes).map_err(|_| io::Error::other("invalid auth JSON"))?;
    // Keep the upstream durable modes; in-memory/external credentials stay isolated.
    use codex_protocol::auth::AuthMode;
    let usable = match auth.resolved_mode() {
        AuthMode::ApiKey => auth
            .openai_api_key
            .as_ref()
            .is_some_and(|key| !key.trim().is_empty()),
        AuthMode::Chatgpt => auth.tokens.as_ref().is_some_and(|tokens| {
            !tokens.access_token.is_empty() && !tokens.refresh_token.is_empty()
        }),
        AuthMode::AgentIdentity => auth
            .agent_identity
            .as_ref()
            .is_some_and(super::storage::AgentIdentityStorage::has_auth_material),
        AuthMode::PersonalAccessToken => auth
            .personal_access_token
            .as_ref()
            .is_some_and(|token| !token.trim().is_empty()),
        AuthMode::BedrockApiKey => auth.bedrock_api_key.is_some(),
        AuthMode::BedrockAccessKeys => auth.bedrock_access_keys.is_some(),
        AuthMode::ChatgptAuthTokens | AuthMode::Headers => false,
    };
    if !usable {
        return Err(io::Error::other(
            "auth JSON is missing usable durable credentials",
        ));
    }
    Ok(auth)
}

#[derive(Debug)]
struct SharedAuthStorage {
    root: PathBuf,
    #[cfg(target_os = "linux")]
    lease: Option<Arc<Lease>>,
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct Lease {
    directory: std::fs::File,
    _lock: std::fs::File,
}

#[cfg(target_os = "linux")]
impl SharedAuthStorage {
    fn directory(&self) -> io::Result<std::fs::File> {
        use rustix::fs::Mode;
        use rustix::fs::OFlags;
        use std::os::unix::fs::MetadataExt;
        let components: Vec<_> = self.root.iter().collect();
        let proc_descriptor = components.len() == 5
            && components[1] == "proc"
            && components[3] == "fd"
            && [components[2], components[4]].iter().all(|part| {
                part.to_str().is_some_and(|value| {
                    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
                })
            });
        if !self.root.is_absolute() || (!proc_descriptor && self.root.canonicalize()? != self.root)
        {
            return Err(io::Error::other(
                "CODEX_AUTH_HOME must be an absolute directory without symlinks",
            ));
        }
        let directory = std::fs::File::from(rustix::fs::open(
            &self.root,
            OFlags::RDONLY
                | OFlags::DIRECTORY
                | OFlags::CLOEXEC
                | if proc_descriptor {
                    OFlags::empty()
                } else {
                    OFlags::NOFOLLOW
                },
            Mode::empty(),
        )?);
        let metadata = directory.metadata()?;
        if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
            return Err(io::Error::other(
                "CODEX_AUTH_HOME must be owned by the current user and private (0700)",
            ));
        }
        if let Some(lease) = &self.lease {
            let held = lease.directory.metadata()?;
            if (held.dev(), held.ino()) != (metadata.dev(), metadata.ino()) {
                return Err(io::Error::other(
                    "CODEX_AUTH_HOME changed during authentication",
                ));
            }
        }
        Ok(directory)
    }

    fn private_file(
        directory: &std::fs::File,
        name: &str,
        flags: rustix::fs::OFlags,
    ) -> io::Result<std::fs::File> {
        use rustix::fs::Mode;
        use rustix::fs::OFlags;
        use std::os::unix::fs::MetadataExt;
        let file = std::fs::File::from(rustix::fs::openat(
            directory,
            name,
            flags | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::RUSR | Mode::WUSR,
        )?);
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o077 != 0
            || metadata.nlink() != 1
        {
            return Err(io::Error::other(
                "shared auth files must be private, owned regular files",
            ));
        }
        Ok(file)
    }
}

#[cfg(target_os = "linux")]
impl AuthStorageBackend for SharedAuthStorage {
    fn load(&self) -> io::Result<Option<AuthDotJson>> {
        let directory = self.directory()?;
        match Self::private_file(&directory, "auth.json", rustix::fs::OFlags::RDONLY) {
            Ok(file) => read_auth_json(file).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn transaction(&self) -> io::Result<Option<Arc<dyn AuthStorageBackend>>> {
        use rustix::fs::OFlags;
        let directory = self.directory()?;
        let lock = Self::private_file(&directory, "auth.lock", OFlags::RDWR | OFlags::CREATE)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "shared auth writer lock timed out",
                    ));
                }
                Err(std::fs::TryLockError::Error(error)) => return Err(error),
            }
        }
        Ok(Some(Arc::new(Self {
            root: self.root.clone(),
            lease: Some(Arc::new(Lease {
                directory,
                _lock: lock,
            })),
        })))
    }

    fn save(&self, auth: &AuthDotJson) -> io::Result<()> {
        use rustix::fs::AtFlags;
        use rustix::fs::OFlags;
        use std::io::Write;
        if self.lease.is_none() {
            return self
                .transaction()?
                .ok_or_else(|| io::Error::other("shared auth transaction unavailable"))?
                .save(auth);
        }
        let directory = self.directory()?;
        let bytes = serde_json::to_vec_pretty(auth)?;
        read_auth_json(bytes.as_slice())?;
        let temporary = format!(".auth-{:016x}.tmp", rand::random::<u64>());
        let mut file = Self::private_file(
            &directory,
            &temporary,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL,
        )?;
        let result = (|| {
            file.write_all(&bytes)?;
            file.sync_all()?;
            rustix::fs::renameat(&directory, &temporary, &directory, "auth.json")?;
            directory.sync_all()
        })();
        if result.is_err() {
            let _ = rustix::fs::unlinkat(&directory, &temporary, AtFlags::empty());
        }
        result
    }

    fn delete(&self) -> io::Result<bool> {
        if self.lease.is_none() {
            return self
                .transaction()?
                .ok_or_else(|| io::Error::other("shared auth transaction unavailable"))?
                .delete();
        }
        let directory = self.directory()?;
        match rustix::fs::unlinkat(&directory, "auth.json", rustix::fs::AtFlags::empty()) {
            Ok(()) => {
                directory.sync_all()?;
                Ok(true)
            }
            Err(rustix::io::Errno::NOENT) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }
}

#[cfg(not(target_os = "linux"))]
impl AuthStorageBackend for SharedAuthStorage {
    fn load(&self) -> io::Result<Option<AuthDotJson>> {
        Err(io::Error::other(
            "CODEX_AUTH_HOME is supported only on Linux",
        ))
    }
    fn save(&self, _: &AuthDotJson) -> io::Result<()> {
        Err(io::Error::other(
            "CODEX_AUTH_HOME is supported only on Linux",
        ))
    }
    fn delete(&self) -> io::Result<bool> {
        Err(io::Error::other(
            "CODEX_AUTH_HOME is supported only on Linux",
        ))
    }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "shared_tests.rs"]
mod tests;
