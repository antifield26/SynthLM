//! IPC endpoint permissions: least-privilege socket files and the same-user
//! default-ACL behavioral assertion.
//!
//! Platform policy (docs/research/D-eng-eco.md §2; `interprocess` 2.4.4 API
//! verified 2026-10-06):
//!
//! - Unix: listeners are bound with `RESTRICTED_SOCKET_MODE` (`0o600`) via
//!   `interprocess::os::unix::local_socket::ListenerOptionsExt::mode`. The OS
//!   gates clients on the socket file's **write** bits (read/execute bits are
//!   cosmetic), so owner-write-only is the minimum that still lets the owning
//!   user connect while denying group/other.
//! - Windows: named pipes are bound with the platform default security
//!   descriptor — no custom `SecurityDescriptor` is set. Setting one would
//!   require raw Windows ACL FFI, which this task forbids (that path must
//!   `BLOCKED` with candidate options instead). The platform default
//!   restricts pipe access to the same user (D-eng-eco §2); the
//!   `default_endpoint_same_user_handshake` test is the behavioral assertion
//!   of that default: same-user connect + `hello` handshake over a
//!   default-bound endpoint.
//!
//! Not covered as an observation (honest list): watching a cross-user /
//! cross-privilege denial happen needs a second OS user, which no single-user
//! test process can mint (TSK-704 BLOCKED). The ignored
//! `cross_user_denial_needs_second_os_user_blocked` test below records the
//! manual two-user procedure and pins the enforcement mechanism instead
//! (per-user temp backing, session-local mapping name).
//!
//! Cross-user posture (TSK-704, verified 2026-10-07 against the pinned
//! sources in the local cargo cache):
//!
//! - Pipes: `interprocess 2.4.4` builds the Windows named-pipe listener with
//!   `security_descriptor: None` (`local_socket/listener/options.rs`), so
//!   `lpSecurityDescriptor` stays NULL
//!   (`os/windows/security_descriptor.rs::create_security_attributes`) and
//!   Windows assigns the creating token's default security descriptor.
//!   Same-user connect therefore works with no custom ACL (asserted by the
//!   test below); what a *different* user gets depends on that
//!   token-default DACL, which no single-user test process can observe.
//! - Segments: `shared_memory 0.12.4` on Windows backs every mapping with a
//!   file under the creating user's temp dir
//!   (`%TEMP%/shared_memory-rs/<os_id>`, `windows.rs::get_tmp_dir`) and
//!   names the mapping without a `Global\` prefix (`windows.rs::new_map`),
//!   so the name lives in the session-local namespace. A different OS user
//!   resolves a different temp dir, so `open` fails before any byte is
//!   touched. The mechanism test below pins both facts against the pinned
//!   backend version.
//!
//! 手动复核指引（需人类在双用户 Windows 上执行，不在 agent 范围内）：
//! 1) 新建第二个本地标准用户 B；2) 以用户 A 运行监听
//! (`bind_default_endpoint` + 握手等待）；3) 以 `runas /user:B`
//! 运行连接端；4) 期望 B 的连接/握手失败（pipe 默认 DACL + shm
//! temp-dir 双重隔离；B 若为管理员可能因默认 DACL 而成功，以实测为准，
//! 不预断）；5) 完成后清理测试用户。shm 段同理：B 打开 A 的
//! descriptor 必为 `Backend` 错误（文件不在 B 的 temp 下）。

use interprocess::local_socket::Listener;

use crate::ipc::bind_endpoint;
#[cfg(unix)]
use crate::ipc::socket_file_path;

/// Least-privilege mode for Unix socket files: owner read/write, nobody else.
///
/// Only the write bits gate clients; `0o600` is the minimum that still lets
/// the owning user connect.
#[cfg(unix)]
pub const RESTRICTED_SOCKET_MODE: u32 = 0o600;

/// Bind endpoint `id` with platform defaults.
///
/// On Windows this creates a named pipe with the default security descriptor
/// (same-user access, asserted by test); on Unix a socket file with default
/// umask permissions. Prefer `bind_restricted_endpoint` on Unix when the
/// socket file must be owner-only regardless of umask.
pub fn bind_default_endpoint(id: &str) -> std::io::Result<Listener> {
    bind_endpoint(id)
}

/// Bind endpoint `id` to a filesystem socket file with owner-only (`0o600`)
/// permissions.
///
/// Always uses a temp-dir socket file (never the Linux abstract namespace,
/// which has no file to chmod), so the mode is observable via
/// [`crate::perm::socket_file_mode_bits`]. Stale socket files are reclaimed at bind time
/// by `interprocess`, like [`crate::ipc::bind_endpoint`].
#[cfg(unix)]
pub fn bind_restricted_endpoint(id: &str) -> std::io::Result<Listener> {
    use interprocess::local_socket::{GenericFilePath, ListenerOptions, ToFsName as _};
    use interprocess::os::unix::local_socket::ListenerOptionsExt as _;

    let name = socket_file_path(id).to_fs_name::<GenericFilePath>()?;
    ListenerOptions::new()
        .name(name)
        .mode(RESTRICTED_SOCKET_MODE as _)
        .create_sync()
}

/// Connect to the filesystem socket file for endpoint `id`.
///
/// Counterpart of [`crate::perm::bind_restricted_endpoint`]: [`crate::ipc::connect_to`]
/// prefers the abstract namespace where supported, so a client for a
/// restricted filesystem socket must connect via the file path explicitly.
#[cfg(unix)]
pub fn connect_to_socket_file(id: &str) -> std::io::Result<interprocess::local_socket::Stream> {
    use interprocess::local_socket::traits::Stream as _;
    use interprocess::local_socket::{GenericFilePath, ToFsName as _};

    let name = socket_file_path(id).to_fs_name::<GenericFilePath>()?;
    interprocess::local_socket::Stream::connect(name)
}

/// Read the permission bits (`& 0o777`) of the socket file at `path`.
#[cfg(unix)]
pub fn socket_file_mode_bits(path: &std::path::Path) -> std::io::Result<u32> {
    use std::os::unix::fs::PermissionsExt as _;

    Ok(std::fs::metadata(path)?.permissions().mode() & 0o777)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_id(tag: &str) -> String {
        format!("synthlm-t115-{tag}-{}", std::process::id())
    }

    /// Same-user connect + handshake over platform defaults.
    ///
    /// On Windows this exercises the named-pipe default ACL: the same user
    /// connects with no custom security descriptor set. On Unix it covers
    /// the filesystem-socket same-user path.
    #[test]
    fn default_endpoint_same_user_handshake() {
        use crate::ipc::{
            EndpointRole, RetryPolicy, accept_next, client_handshake, connect_to_with_retry,
            server_handshake,
        };

        let id = unique_id("default");
        let listener = bind_default_endpoint(&id).expect("bind default endpoint");
        let server = std::thread::spawn(move || {
            let mut conn = accept_next(&listener).expect("accept");
            server_handshake(&mut conn, EndpointRole::Acrd).expect("server handshake")
        });
        let policy = RetryPolicy {
            max_attempts: 50,
            base_delay_ms: 10,
            max_delay_ms: 50,
        };
        let mut client = connect_to_with_retry(&id, &policy).expect("same-user connect");
        let peer = client_handshake(&mut client, EndpointRole::Bridge).expect("client handshake");
        assert_eq!(peer.role, EndpointRole::Acrd);
        let ours = server.join().expect("server thread");
        assert_eq!(ours.role, EndpointRole::Bridge);
    }

    /// Unix least privilege: the socket file is owner-only and the owning
    /// user can connect and handshake over it.
    #[cfg(unix)]
    #[test]
    fn restricted_socket_file_is_owner_only_and_same_user_connects() {
        use crate::ipc::{EndpointRole, accept_next, client_handshake, server_handshake};
        use std::io::ErrorKind;

        let id = unique_id("restricted");
        let listener = match bind_restricted_endpoint(&id) {
            Ok(listener) => listener,
            Err(e) if e.kind() == ErrorKind::Unsupported => {
                eprintln!("SKIP: socket file mode unsupported on this Unix; recorded as uncovered");
                return;
            }
            Err(e) => panic!("bind restricted endpoint: {e}"),
        };
        let path = socket_file_path(&id);
        let bits = socket_file_mode_bits(&path).expect("stat socket file");
        assert_eq!(
            bits, RESTRICTED_SOCKET_MODE,
            "socket file must be owner-only (0o600), got 0o{bits:o}"
        );

        let server = std::thread::spawn(move || {
            let mut conn = accept_next(&listener).expect("accept");
            server_handshake(&mut conn, EndpointRole::Acrd).expect("server handshake")
        });
        let mut client = connect_to_socket_file(&id).expect("same-user connect");
        let peer = client_handshake(&mut client, EndpointRole::Ui).expect("client handshake");
        assert_eq!(peer.role, EndpointRole::Acrd);
        let ours = server.join().expect("server thread");
        assert_eq!(ours.role, EndpointRole::Ui);
        drop(client);
        let _ = std::fs::remove_file(&path);
    }

    /// Cross-user denial: BLOCKED without a second OS user (TSK-704).
    ///
    /// `#[ignore]`d on purpose: a passing default run must never imply a
    /// denial was observed. Without `SYNTHLM_TEST_CROSS_USER_MANUAL=1` this
    /// test prints the BLOCKED guidance and returns; with the flag set (the
    /// operator asserts the manual two-user run from the module docs
    /// happened) it pins the enforcement mechanism this single-token
    /// process *can* see — per-user temp backing plus a session-local
    /// mapping name — which is what makes a different user's `open` fail
    /// before any byte is touched. It never claims a denial it did not
    /// observe.
    #[test]
    #[ignore]
    fn cross_user_denial_needs_second_os_user_blocked() {
        if std::env::var("SYNTHLM_TEST_CROSS_USER_MANUAL").as_deref() != Ok("1") {
            eprintln!(
                "BLOCKED (TSK-704): observing a cross-user denial needs two OS users. \
                Single-user CI cannot mint a second token, so no denial is asserted here. \
                Manual procedure (human, dual-user Windows): create a second local standard \
                user B; bind + listen as user A; connect as B via `runas /user:B` and expect \
                the connect/handshake to fail (pipe default DACL + per-user shm temp dir); \
                clean up the test user afterwards. See the module docs in `perm`."
            );
            return;
        }
        // Mechanism pins against the pinned backend (`shared_memory 0.12.4`,
        // Windows backend): the backing file must live under *this* user's
        // temp dir (so another user resolves a different dir and fails
        // open), and the mapping name must carry no `Global\` prefix (so it
        // stays in the session-local namespace). No absolute path is
        // printed (AGENTS.md §8); only the properties are asserted.
        let sender = crate::shm::ShmSender::create(b"cross-user mechanism probe")
            .expect("create probe segment");
        let os_id = sender.descriptor().shm_name.clone();
        assert!(
            !os_id.contains('\\'),
            "mapping name must carry no namespace prefix (session-local), got {os_id:?}"
        );
        let backing = std::env::temp_dir()
            .join("shared_memory-rs")
            .join(os_id.trim_start_matches('/'));
        assert!(
            backing.is_file(),
            "segment must be backed under the creating user's temp dir"
        );
    }
}
