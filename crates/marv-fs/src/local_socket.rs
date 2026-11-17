//! A local socket at a path: what the mesh, the processes' relay and the harness mesh listen on and connect to.
//!
//! On Unix these are tokio's Unix sockets, unchanged (the names are aliases).
//!
//! On Windows, where tokio has no Unix sockets, the listener is a named pipe (tokio's safe
//! `named_pipe::{ServerOptions, ClientOptions}`), and the path holds a small marker file instead:
//! `marv-pipe <pipe name> <token>`, written once (it fails if the path exists, as a Unix bind does). Code that checks
//! the path exists, scans a folder of sockets, removes a stale one or rebinds works the same on both.
//!
//! - **The pipe's name** is `\\.\pipe\marv-<hash of the path>-<random>`. The hash says which socket it is; the random
//!   part lets a path be rebound while the old listener is still shutting down (a plug unplugged and plugged again),
//!   which a name derived from the path alone would refuse. Clients read the name from the marker, never derive it.
//! - **Who gets in.** Setting an owner-only security descriptor needs `unsafe` FFI, which this crate denies, so the
//!   pipe keeps Windows' default DACL: full control for SYSTEM, Administrators and the creator owner, read-only for
//!   everyone else. `first_pipe_instance(true)` makes the bind fail if anyone already owns the name (no squatting),
//!   and `reject_remote_clients(true)` keeps it on this machine. On top of that, a connection is taken only after it
//!   sends the 64-hex-digit token from the marker as its first line, so only a process that can read the file (the
//!   user's own: it lives under their profile) gets in. Another user can at most open the pipe read-only, can't send
//!   the token, and is dropped after five seconds without the server ever writing to it.
//! - **Buffers are 64 KiB** each way: the default 512 B stalled busy streams behind the reader in herdr's Windows
//!   port, which this follows.
//! - **The token is read as exactly 65 bytes** (no buffered reader), so a client's first request sent right behind it
//!   is never swallowed. (The earlier TCP version read it through a `BufReader` and could drop such bytes.)
//! - **No half-close.** A pipe can't shut only its write side. An async stream's `shutdown` flushes; the other end
//!   sees the end once both halves are dropped. [`BlockingStream::shutdown`] ends the connection on this end: reads
//!   then return end-of-file, so `__mesh-mcp` finishes when its stdin does (on Unix it waits for the server to close).
//! - **The peer's pid** comes from `GetNamedPipeClientProcessId` / `GetNamedPipeServerProcessId` ([`peer_pid`]): the
//!   workspace's second audited `unsafe` (`windows::pipe_peer_pid`; the first is marv-cli's `sigaction` query). Without
//!   it the mesh couldn't confirm who called a door (`confirm_caller`), and every send between chats failed on
//!   Windows ("can't confirm the caller", 2026-11-14).
//! - Behaviour change from the TCP version (2026-11): a marker of the old `marv-tcp` format isn't a socket any more,
//!   so it's treated as stale and replaced; an older Marv and this one don't reach each other's sockets on Windows.

#[cfg(unix)]
pub use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
#[cfg(unix)]
pub use tokio::net::{UnixListener as Listener, UnixStream as Stream};

#[cfg(windows)]
pub use windows::{BlockingStream, Listener, OwnedReadHalf, OwnedWriteHalf, StdListener, Stream};

/// A listener bound outside a tokio runtime, served later ([`StdListener::into_tokio`]).
#[cfg(unix)]
#[derive(Debug)]
pub struct StdListener(std::os::unix::net::UnixListener);

#[cfg(unix)]
impl StdListener {
    /// Binds `path`, private to this user (the socket made 0600).
    pub fn bind_private(path: &std::path::Path) -> std::io::Result<Self> {
        let listener = std::os::unix::net::UnixListener::bind(path)?;
        crate::mode::set_mode(path, 0o600)?;
        Ok(Self(listener))
    }

    /// Inside a tokio runtime.
    pub fn into_tokio(self) -> std::io::Result<Listener> {
        self.0.set_nonblocking(true)?;
        Listener::from_std(self.0)
    }
}

/// Whether `path` is a local socket (Unix: a socket file; Windows: a pipe's marker file).
pub fn is_socket(meta: &std::fs::Metadata, path: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        let _ = path;
        std::os::unix::fs::FileTypeExt::is_socket(&meta.file_type())
    }
    #[cfg(windows)]
    {
        meta.is_file() && windows::read_marker(path).is_some()
    }
}

/// The marker's pieces: kept out of the Windows module so the parsing and naming are tested on every platform.
#[cfg_attr(not(windows), allow(dead_code))]
mod marker {
    pub(crate) const MAGIC: &str = "marv-pipe";
    /// Every Marv pipe's name starts so; a marker naming anything else (another program's pipe) is refused.
    pub(crate) const PIPE_PREFIX: &str = r"\\.\pipe\marv-";
    /// 32 random bytes in hex.
    pub(crate) const TOKEN_LEN: usize = 64;

    /// FNV-1a over the path's bytes: stable across Rust versions and processes, unlike `DefaultHasher`.
    pub(crate) fn path_hash(path: &std::path::Path) -> u64 {
        path.as_os_str()
            .as_encoded_bytes()
            .iter()
            .fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3))
    }

    /// The pipe's name for a socket at `path`; `salt` is random per bind.
    pub(crate) fn pipe_name(path: &std::path::Path, salt: u32) -> String {
        format!("{PIPE_PREFIX}{:016x}-{salt:08x}", path_hash(path))
    }

    pub(crate) fn format(name: &str, token: &str) -> String {
        format!("{MAGIC} {name} {token}\n")
    }

    /// `(pipe name, token)` from a marker's text, if it's one of ours.
    pub(crate) fn parse(text: &str) -> Option<(String, String)> {
        let mut parts = text.split_whitespace();
        (parts.next()? == MAGIC).then_some(())?;
        let name = parts.next()?;
        let token = parts.next()?;
        let name_ok = name
            .strip_prefix(PIPE_PREFIX)
            .is_some_and(|rest| !rest.is_empty() && rest.len() <= 64 && rest.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'));
        let token_ok = token.len() == TOKEN_LEN && token.bytes().all(|b| b.is_ascii_hexdigit());
        (name_ok && token_ok && parts.next().is_none()).then(|| (name.to_string(), token.to_string()))
    }
}

#[cfg(windows)]
mod windows {
    use std::io;
    use std::path::Path;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::task::{Context, Poll};
    use std::time::Duration;

    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions};

    use super::marker;

    /// How long a new connection has to send its token.
    pub(super) const HELLO: Duration = Duration::from_secs(5);

    /// The pid of the process at the other end of a named pipe: its client's on the server's end (`server_end`), its
    /// server's on a client's end. Windows says it only through these kernel32 calls, so this is the workspace's
    /// second audited `unsafe` (marv-cli's `sigaction` query is the first; the person approved this one, 2026-11-14,
    /// and marv-cli's guard test allows exactly these two). None when the call fails.
    ///
    /// Safety: `handle` is a live pipe handle borrowed from a stream the caller holds across the call (never closed or
    /// moved here), `pid` is a valid out-parameter the calls only write, and both calls are documented to accept any
    /// named-pipe handle and to return 0 on failure without touching anything else.
    #[allow(unsafe_code)]
    pub(super) fn pipe_peer_pid(handle: std::os::windows::io::RawHandle, server_end: bool) -> Option<i32> {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetNamedPipeClientProcessId(pipe: std::os::windows::io::RawHandle, client_process_id: *mut u32) -> i32;
            fn GetNamedPipeServerProcessId(pipe: std::os::windows::io::RawHandle, server_process_id: *mut u32) -> i32;
        }
        let mut pid: u32 = 0;
        let ok = unsafe {
            if server_end { GetNamedPipeClientProcessId(handle, &mut pid) } else { GetNamedPipeServerProcessId(handle, &mut pid) }
        };
        if ok == 0 || pid == 0 { None } else { i32::try_from(pid).ok() }
    }
    /// Each direction's pipe buffer (see the module doc: 512 B stalls).
    const BUFFER: u32 = 64 * 1024;
    /// `ERROR_PIPE_BUSY`: every instance is taken for the moment; the server makes a new one right after an accept.
    const ERROR_PIPE_BUSY: i32 = 231;
    /// How long a connect keeps retrying a busy pipe.
    const BUSY_WAIT: Duration = Duration::from_secs(2);
    const BUSY_RETRY: Duration = Duration::from_millis(20);

    /// What [`Listener::accept`] gives besides the stream: a pipe client has no address (the TCP version's was
    /// 127.0.0.1 and a port, which no caller read).
    pub type PeerAddr = ();

    pub(super) fn read_marker(path: &Path) -> Option<(String, String)> {
        marker::parse(&std::fs::read_to_string(path).ok()?)
    }

    fn random<const N: usize>() -> io::Result<[u8; N]> {
        let mut bytes = [0u8; N];
        getrandom::fill(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
        Ok(bytes)
    }

    fn token() -> io::Result<String> {
        Ok(random::<32>()?.iter().map(|b| format!("{b:02x}")).collect())
    }

    fn server_options(first: bool) -> ServerOptions {
        let mut options = ServerOptions::new();
        options.first_pipe_instance(first).reject_remote_clients(true).in_buffer_size(BUFFER).out_buffer_size(BUFFER);
        options
    }

    /// Opens the client end, retrying while every instance is busy (the server is between an accept and its next
    /// instance). Needs a tokio runtime.
    async fn open_client(name: &str) -> io::Result<NamedPipeClient> {
        let deadline = tokio::time::Instant::now() + BUSY_WAIT;
        loop {
            match ClientOptions::new().open(name) {
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(BUSY_RETRY).await;
                }
                opened => return opened,
            }
        }
    }

    fn not_ours() -> io::Error {
        io::Error::new(io::ErrorKind::ConnectionRefused, "not a Marv socket")
    }

    /// One end of a connection: the server's accepted instance or a client. Both are read and written alike.
    #[derive(Debug)]
    pub enum Stream {
        Server(NamedPipeServer),
        Client(NamedPipeClient),
    }

    pub type OwnedReadHalf = tokio::io::ReadHalf<Stream>;
    pub type OwnedWriteHalf = tokio::io::WriteHalf<Stream>;

    impl Stream {
        /// Read and write halves, usable from different tasks (as a Unix stream's `into_split`).
        pub fn into_split(self) -> (OwnedReadHalf, OwnedWriteHalf) {
            tokio::io::split(self)
        }
    }

    impl AsyncRead for Stream {
        fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
            match self.get_mut() {
                Stream::Server(s) => Pin::new(s).poll_read(cx, buf),
                Stream::Client(c) => Pin::new(c).poll_read(cx, buf),
            }
        }
    }

    impl AsyncWrite for Stream {
        fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
            match self.get_mut() {
                Stream::Server(s) => Pin::new(s).poll_write(cx, buf),
                Stream::Client(c) => Pin::new(c).poll_write(cx, buf),
            }
        }

        fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            match self.get_mut() {
                Stream::Server(s) => Pin::new(s).poll_flush(cx),
                Stream::Client(c) => Pin::new(c).poll_flush(cx),
            }
        }

        fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            match self.get_mut() {
                Stream::Server(s) => Pin::new(s).poll_shutdown(cx),
                Stream::Client(c) => Pin::new(c).poll_shutdown(cx),
            }
        }
    }

    /// The listening side: the instance the next client will land on, swapped for a fresh one at each accept.
    #[derive(Debug)]
    pub struct Listener {
        name: String,
        token: String,
        next: tokio::sync::Mutex<NamedPipeServer>,
    }

    /// A listener bound outside a tokio runtime, served later ([`Self::into_tokio`]).
    ///
    /// A pipe instance can only be made inside a runtime (tokio registers it with its reactor), so when
    /// [`Self::bind_private`] runs outside one, the marker is written at once and the first instance is made in
    /// [`Self::into_tokio`]: until then a client finds the marker but not the pipe, and its connect fails (a Unix
    /// socket would queue it).
    #[derive(Debug)]
    pub struct StdListener {
        name: String,
        token: String,
        first: Option<NamedPipeServer>,
    }

    impl StdListener {
        /// Makes the pipe and writes `path` (refused if it exists: remove a stale one first, as on Unix).
        pub fn bind_private(path: &Path) -> io::Result<Self> {
            let name = marker::pipe_name(path, u32::from_le_bytes(random::<4>()?));
            let token = token()?;
            let first = match tokio::runtime::Handle::try_current() {
                Ok(_) => Some(server_options(true).create(&name)?),
                Err(_) => None,
            };
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
            file.write_all(marker::format(&name, &token).as_bytes())?;
            Ok(Self { name, token, first })
        }

        /// Inside a tokio runtime.
        pub fn into_tokio(self) -> io::Result<Listener> {
            let first = match self.first {
                Some(first) => first,
                None => server_options(true).create(&self.name)?,
            };
            Ok(Listener { name: self.name, token: self.token, next: tokio::sync::Mutex::new(first) })
        }
    }

    impl Listener {
        /// Makes the pipe and writes `path` (refused if it exists: remove a stale one first, as on Unix).
        pub fn bind(path: impl AsRef<Path>) -> io::Result<Self> {
            StdListener::bind_private(path.as_ref())?.into_tokio()
        }

        /// The next connection that sent the token; others are dropped. Waiting for a client is cancel-safe (tokio's
        /// `connect`); a client cancelled while its token is being read is dropped.
        pub async fn accept(&self) -> io::Result<(Stream, PeerAddr)> {
            loop {
                let mut connected = {
                    let mut next = self.next.lock().await;
                    next.connect().await?;
                    let fresh = server_options(false).create(&self.name)?;
                    std::mem::replace(&mut *next, fresh)
                };
                let mut said = [0u8; marker::TOKEN_LEN + 1];
                let heard = tokio::time::timeout(HELLO, connected.read_exact(&mut said)).await;
                let (token, newline) = said.split_at(marker::TOKEN_LEN);
                if matches!(heard, Ok(Ok(_)))
                    && newline == b"\n"
                    && std::str::from_utf8(token).is_ok_and(|t| crate::mode::same_secret(t, &self.token))
                {
                    return Ok((Stream::Server(connected), ()));
                }
            }
        }
    }

    /// Connects to the pipe whose marker is `path`, and says its token.
    pub async fn connect(path: impl AsRef<Path>) -> io::Result<Stream> {
        let (name, token) = read_marker(path.as_ref()).ok_or_else(not_ours)?;
        let mut client = open_client(&name).await?;
        client.write_all(format!("{token}\n").as_bytes()).await?;
        Ok(Stream::Client(client))
    }

    /// Whether nothing listens behind the marker at `path`: not a marker, or no pipe of that name. A busy pipe
    /// (every instance taken) or one we may not open is someone's, so alive.
    pub(super) fn nobody_listens(path: &Path) -> bool {
        let Some((name, _)) = read_marker(path) else { return true };
        // A plain open is a pipe client (CreateFile); dropped at once, the server's token wait drops it in turn.
        match std::fs::OpenOptions::new().read(true).write(true).open(&name) {
            Ok(_) => false,
            Err(e) => e.kind() == io::ErrorKind::NotFound,
        }
    }

    struct Shared {
        // Dropped before the runtime it's registered with.
        pipe: NamedPipeClient,
        closed: tokio::sync::watch::Sender<bool>,
        runtime: tokio::runtime::Runtime,
    }

    /// A blocking connection (for a helper process that only pipes bytes), with `try_clone` and `shutdown` as a
    /// Unix stream has.
    ///
    /// Not a `std::fs::File` on the pipe: a synchronous handle serializes its I/O, so one thread blocked reading would
    /// hold up the other thread's write (`__mesh-mcp` copies both ways at once). Instead the overlapped client runs on
    /// a one-worker runtime of its own and each call blocks on it; clones share it.
    #[derive(Clone)]
    pub struct BlockingStream(Arc<Shared>);

    impl std::fmt::Debug for BlockingStream {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("BlockingStream").field("pipe", &self.0.pipe).finish_non_exhaustive()
        }
    }

    impl BlockingStream {
        pub(super) fn connect(path: &Path) -> io::Result<Self> {
            use std::io::Write;
            let (name, token) = read_marker(path).ok_or_else(not_ours)?;
            let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_io().enable_time().build()?;
            let pipe = runtime.block_on(open_client(&name))?;
            let (closed, _) = tokio::sync::watch::channel(false);
            let mut stream = Self(Arc::new(Shared { pipe, closed, runtime }));
            stream.write_all(format!("{token}\n").as_bytes())?;
            Ok(stream)
        }

        /// Another handle on the same connection.
        pub fn try_clone(&self) -> io::Result<Self> {
            Ok(self.clone())
        }

        /// Ends the connection on this end, whichever side is named: a pipe has no half-close (see the module doc).
        /// Reads then return end-of-file and writes fail; the handle closes when the last clone is dropped.
        pub fn shutdown(&self, _how: std::net::Shutdown) -> io::Result<()> {
            self.0.closed.send_replace(true);
            Ok(())
        }
    }

    impl io::Read for BlockingStream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let shared = &*self.0;
            let mut closed = shared.closed.subscribe();
            shared.runtime.block_on(async {
                loop {
                    if *closed.borrow_and_update() {
                        return Ok(0);
                    }
                    tokio::select! {
                        ready = shared.pipe.readable() => {
                            ready?;
                            match shared.pipe.try_read(buf) {
                                Ok(n) => return Ok(n),
                                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                                // The server went away: the end of the stream, as a closed Unix socket reads.
                                Err(e) if e.kind() == io::ErrorKind::BrokenPipe => return Ok(0),
                                Err(e) => return Err(e),
                            }
                        }
                        _ = closed.changed() => {}
                    }
                }
            })
        }
    }

    impl io::Write for BlockingStream {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let shared = &*self.0;
            let mut closed = shared.closed.subscribe();
            shared.runtime.block_on(async {
                loop {
                    if *closed.borrow_and_update() {
                        return Err(io::Error::from(io::ErrorKind::BrokenPipe));
                    }
                    tokio::select! {
                        ready = shared.pipe.writable() => {
                            ready?;
                            match shared.pipe.try_write(buf) {
                                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                                written => return written,
                            }
                        }
                        _ = closed.changed() => {}
                    }
                }
            })
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
}

/// Connects to the local socket at `path`.
pub async fn connect(path: impl AsRef<std::path::Path>) -> std::io::Result<Stream> {
    #[cfg(unix)]
    return Stream::connect(path.as_ref()).await;
    #[cfg(windows)]
    return windows::connect(path).await;
}

/// The process at the other end of `stream`: Unix's peer credentials; on Windows the named pipe's client (on the
/// server's accepted end) or server (on a client's end), as `windows::pipe_peer_pid` asks the kernel.
pub fn peer_pid(stream: &Stream) -> Option<i32> {
    #[cfg(unix)]
    return stream.peer_cred().ok().and_then(|c| c.pid());
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        match stream {
            Stream::Server(pipe) => windows::pipe_peer_pid(pipe.as_raw_handle(), true),
            Stream::Client(pipe) => windows::pipe_peer_pid(pipe.as_raw_handle(), false),
        }
    }
}

/// Listens at `path`, private to this user (Unix: the socket made 0600), inside a tokio runtime.
pub fn bind_private(path: &std::path::Path) -> std::io::Result<Listener> {
    StdListener::bind_private(path)?.into_tokio()
}

/// Windows: whether nothing listens behind the marker at `path` (no such pipe, or not a marker at all). Unix callers
/// probe the socket itself.
#[cfg(windows)]
pub fn nobody_listens(path: &std::path::Path) -> bool {
    windows::nobody_listens(path)
}

/// A blocking connection (for a helper process that only pipes bytes): `try_clone` and `shutdown` work on both.
#[cfg(unix)]
pub type BlockingStream = std::os::unix::net::UnixStream;

/// Connects to the local socket at `path`, blocking.
pub fn connect_blocking(path: &std::path::Path) -> std::io::Result<BlockingStream> {
    #[cfg(unix)]
    return BlockingStream::connect(path);
    #[cfg(windows)]
    return BlockingStream::connect(path);
}

#[cfg(test)]
mod tests {
    use super::marker;

    #[test]
    fn a_marker_names_a_marv_pipe_and_a_token() {
        let path = std::path::Path::new(r"C:\Users\me\AppData\Local\marv\mesh\abc.sock");
        let name = marker::pipe_name(path, 0xdead_beef);
        assert!(name.starts_with(marker::PIPE_PREFIX) && name.ends_with("-deadbeef"), "{name}");
        assert_eq!(name, marker::pipe_name(path, 0xdead_beef), "the same path and salt name the same pipe");
        assert_ne!(name, marker::pipe_name(std::path::Path::new("other.sock"), 0xdead_beef));
        let token = "ab".repeat(32);
        let text = marker::format(&name, &token);
        assert_eq!(marker::parse(&text), Some((name.clone(), token.clone())));
    }

    #[test]
    fn a_marker_that_isnt_ours_is_refused() {
        let token = "0f".repeat(32);
        for text in [
            format!(r"marv-tcp 4000 {token}"),
            format!(r"marv-pipe \\.\pipe\herdr {token}"),
            format!(r"marv-pipe \\server\pipe\marv-1 {token}"),
            format!(r"marv-pipe \\.\pipe\marv-../x {token}"),
            format!(r"marv-pipe \\.\pipe\marv- {token}"),
            r"marv-pipe \\.\pipe\marv-1 short".to_string(),
            format!(r"marv-pipe \\.\pipe\marv-1 {token} extra"),
            String::new(),
        ] {
            assert_eq!(marker::parse(&text), None, "{text}");
        }
    }

    #[test]
    fn the_path_hash_is_stable() {
        // FNV-1a's published value for "a": a socket's pipe name starts the same in every build, so a pipe list can be
        // read against the folder. (Clients take the name from the marker, never recompute it.)
        assert_eq!(marker::path_hash(std::path::Path::new("a")), 0xaf63_dc4c_8601_ec8c);
    }

    /// Each end of a connection says the other's pid (here both are this process): the mesh confirms who called a door
    /// by it. On Windows it was `None`, and every send between chats failed the caller check.
    #[tokio::test]
    async fn each_end_knows_the_other_ends_pid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("door.sock");
        let listener = super::bind_private(&path).unwrap();
        let (client, accepted) = tokio::join!(super::connect(&path), listener.accept());
        let (client, (server, _)) = (client.unwrap(), accepted.unwrap());
        let me = i32::try_from(std::process::id()).unwrap();
        assert_eq!(super::peer_pid(&server), Some(me), "the door sees its caller");
        assert_eq!(super::peer_pid(&client), Some(me), "the caller sees the door");
    }
}
