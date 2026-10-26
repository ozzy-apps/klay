//! Private per-user local delivery. A successful reply means Studio applied the snapshot.
use std::{
    path::{Path, PathBuf},
    sync::mpsc,
};

#[derive(Debug)]
pub enum SendError {
    Unavailable,
    Rejected(String),
    Transport(String),
}
impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => write!(f, "Strata is not running"),
            Self::Rejected(s) | Self::Transport(s) => write!(f, "{s}"),
        }
    }
}
pub enum Incoming {
    Snapshot {
        package: crate::Package,
        update_only: bool,
        reply: mpsc::Sender<Result<usize, String>>,
    },
    Focus {
        reply: mpsc::Sender<Result<usize, String>>,
    },
}
#[cfg(unix)]
mod platform {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::{
        io::{BufRead, BufReader, Read, Write},
        os::unix::{
            fs::{FileTypeExt, MetadataExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };
    #[derive(Serialize, Deserialize)]
    struct Request {
        path: Option<PathBuf>,
        update_only: bool,
    }
    #[derive(Serialize, Deserialize)]
    struct Reply {
        accepted: usize,
        error: Option<String>,
    }
    pub struct Server {
        pub incoming: mpsc::Receiver<Incoming>,
        stop: Arc<AtomicBool>,
        socket: PathBuf,
    }
    pub fn directory() -> Result<PathBuf, String> {
        // getuid has no memory or process side effects.
        let uid = unsafe { libc::getuid() };
        let root = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join(format!("strata-runtime-{uid}")));
        secure_directory(&root)?;
        let path = root.join("strata-studio");
        secure_directory(&path)?;
        Ok(path)
    }
    fn secure_directory(path: &Path) -> Result<(), String> {
        if !path.exists() {
            std::fs::create_dir_all(path).map_err(|e| e.to_string())?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
        let m = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        if !m.is_dir()
            || m.uid() != unsafe { libc::getuid() }
            || m.permissions().mode() & 0o077 != 0
        {
            return Err(
                "Strata bridge directory must be private and owned by the current user".into(),
            );
        }
        Ok(())
    }
    fn socket(path: &Path) -> PathBuf {
        path.join("bridge.sock")
    }
    fn same_user(stream: &UnixStream) -> bool {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            let mut cred = libc::ucred {
                pid: 0,
                uid: 0,
                gid: 0,
            };
            let mut size = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
            // The kernel writes one ucred into an exactly sized, live stack buffer.
            unsafe {
                libc::getsockopt(
                    stream.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_PEERCRED,
                    (&mut cred as *mut libc::ucred).cast(),
                    &mut size,
                ) == 0
                    && cred.uid == libc::getuid()
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = stream;
            true
        }
    }
    fn bounded_line(stream: &mut UnixStream) -> Result<String, String> {
        let mut line = String::new();
        BufReader::new(stream.take(8193))
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        if line.len() > 8192 || !line.ends_with('\n') {
            return Err("Invalid bridge message".into());
        }
        Ok(line)
    }
    pub fn running_at(dir: &Path) -> bool {
        UnixStream::connect(socket(dir)).is_ok()
    }
    impl Server {
        pub fn bind(dir: &Path) -> Result<Self, String> {
            secure_directory(dir)?;
            let endpoint = socket(dir);
            if endpoint.exists() {
                if running_at(dir) {
                    return Err("Strata bridge is already running".into());
                }
                let m = std::fs::symlink_metadata(&endpoint).map_err(|e| e.to_string())?;
                if !m.file_type().is_socket() || m.uid() != unsafe { libc::getuid() } {
                    return Err("Unsafe bridge endpoint".into());
                }
                std::fs::remove_file(&endpoint).map_err(|e| e.to_string())?;
            }
            let listener = UnixListener::bind(&endpoint).map_err(|e| e.to_string())?;
            std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| e.to_string())?;
            let (tx, rx) = mpsc::sync_channel(8);
            let stop = Arc::new(AtomicBool::new(false));
            let flag = stop.clone();
            let directory = dir.to_path_buf();
            std::thread::spawn(move || {
                for connection in listener.incoming() {
                    if flag.load(Ordering::Relaxed) {
                        break;
                    }
                    let Ok(mut stream) = connection else {
                        break;
                    };
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                    let result = (|| -> Result<usize, String> {
                        if !same_user(&stream) {
                            return Err("Bridge peer has a different user".into());
                        }
                        let req: Request = serde_json::from_str(&bounded_line(&mut stream)?)
                            .map_err(|e| e.to_string())?;
                        let (reply_tx, reply_rx) = mpsc::channel();
                        if let Some(path) = req.path {
                            let meta =
                                std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
                            if !meta.is_file()
                                || meta.uid() != unsafe { libc::getuid() }
                                || path.extension().is_none_or(|e| e != "stratabridge")
                                || path.parent().and_then(|p| p.canonicalize().ok())
                                    != directory.canonicalize().ok()
                            {
                                return Err("Bridge package must be a regular file in the private transfer directory".into());
                            }
                            let package = crate::read_package(&path)?;
                            tx.send(Incoming::Snapshot {
                                package,
                                update_only: req.update_only,
                                reply: reply_tx,
                            })
                            .map_err(|_| "Strata receiver closed")?;
                            let result = reply_rx
                                .recv_timeout(Duration::from_secs(45))
                                .map_err(|_| "Strata did not acknowledge the transfer")?;
                            if result.is_ok() {
                                let _ = std::fs::remove_file(path);
                            }
                            result
                        } else {
                            tx.send(Incoming::Focus { reply: reply_tx })
                                .map_err(|_| "Strata receiver closed")?;
                            reply_rx
                                .recv_timeout(Duration::from_secs(5))
                                .map_err(|_| "Strata did not respond")?
                        }
                    })();
                    let reply = match result {
                        Ok(accepted) => Reply {
                            accepted,
                            error: None,
                        },
                        Err(e) => Reply {
                            accepted: 0,
                            error: Some(e),
                        },
                    };
                    if let Ok(mut bytes) = serde_json::to_vec(&reply) {
                        bytes.push(b'\n');
                        let _ = stream.write_all(&bytes);
                    }
                }
            });
            Ok(Self {
                incoming: rx,
                stop,
                socket: endpoint,
            })
        }
    }
    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            let _ = UnixStream::connect(&self.socket);
            let _ = std::fs::remove_file(&self.socket);
        }
    }
    pub fn send_at(dir: &Path, path: Option<&Path>, update_only: bool) -> Result<usize, SendError> {
        let mut stream = UnixStream::connect(socket(dir)).map_err(|e| {
            if [
                std::io::ErrorKind::NotFound,
                std::io::ErrorKind::ConnectionRefused,
            ]
            .contains(&e.kind())
            {
                SendError::Unavailable
            } else {
                SendError::Transport(e.to_string())
            }
        })?;
        if !same_user(&stream) {
            return Err(SendError::Transport(
                "Bridge peer has a different user".into(),
            ));
        }
        stream
            .set_read_timeout(Some(Duration::from_secs(50)))
            .map_err(|e| SendError::Transport(e.to_string()))?;
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| SendError::Transport(e.to_string()))?;
        let request = Request {
            path: path.map(Path::to_path_buf),
            update_only,
        };
        let mut bytes =
            serde_json::to_vec(&request).map_err(|e| SendError::Transport(e.to_string()))?;
        bytes.push(b'\n');
        stream
            .write_all(&bytes)
            .map_err(|e| SendError::Transport(e.to_string()))?;
        let reply: Reply =
            serde_json::from_str(&bounded_line(&mut stream).map_err(SendError::Transport)?)
                .map_err(|e| SendError::Transport(e.to_string()))?;
        if let Some(e) = reply.error {
            Err(SendError::Rejected(e))
        } else {
            Ok(reply.accepted)
        }
    }
}
#[cfg(unix)]
pub use platform::{Server, directory, running_at, send_at};
#[cfg(not(unix))]
pub fn directory() -> Result<PathBuf, String> {
    Err("Direct bridge currently requires Linux / Unix".into())
}
#[cfg(not(unix))]
pub fn send_at(_: &Path, _: Option<&Path>, _: bool) -> Result<usize, SendError> {
    Err(SendError::Transport(
        "Direct bridge currently requires Linux / Unix".into(),
    ))
}

#[cfg(not(unix))]
pub struct Server {
    pub incoming: mpsc::Receiver<Incoming>,
}
#[cfg(not(unix))]
impl Server {
    pub fn bind(_: &Path) -> Result<Self, String> {
        Err("Direct bridge currently requires Linux / Unix".into())
    }
}
#[cfg(not(unix))]
pub fn running_at(_: &Path) -> bool {
    false
}
