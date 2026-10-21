mod window;
pub use window::PairingWindow;
#[cfg(feature = "pairing")]
mod service;
#[cfg(test)]
mod tests;
#[cfg(feature = "pairing")]
pub(crate) mod transport;
#[cfg(feature = "pairing")]
pub use service::PairingService;
