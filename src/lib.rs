//! XMouse: play your own Xbox with mouse and keyboard over Xbox remote play.

pub mod audio;
pub mod auth;
pub mod mapper;
pub mod packet;
pub mod profile;
pub mod storage;
pub mod stream;
pub mod video;
#[cfg(windows)]
pub mod vsr;
pub mod xhome;

/// Pick the TLS crypto backend once (several dependencies enable different ones).
pub fn init_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

pub mod app;
pub mod keyboard;
pub mod padview;
pub mod theme;
