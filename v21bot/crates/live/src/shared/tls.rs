//! TLS is rustls on ring: aws-lc, rustls' default, needs CMake and NASM to build on Windows. The
//! websockets find ring by the crate feature; reqwest needs it installed for the process, once.

/// Installs ring as the process's crypto provider; a second call does nothing.
pub fn init() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
