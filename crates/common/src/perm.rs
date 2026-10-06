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
//! Not covered (honest list): cross-user / cross-privilege denial. Refusing a
//! *different* OS user needs a second OS user and cannot be exercised from a
//! unit test; it is recorded as uncovered in the task return notes.

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
/// [`socket_file_mode_bits`]. Stale socket files are reclaimed at bind time
/// by `interprocess`, like [`bind_endpoint`].
#[cfg(unix)]
pub fn bind_restricted_endpoint(id: &str) -> std::io::Result<Listener> {
    use interprocess::local_socket::{GenericFilePath, ListenerOptions};
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
    use interprocess::local_socket::GenericFilePath;

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
}
