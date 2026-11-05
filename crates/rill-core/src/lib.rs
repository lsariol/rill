//! Platform-neutral chat core for Rill. Nothing here depends on the UI or the OS.

pub mod destination;
pub mod platform;

pub use destination::Destination;
pub use platform::Platform;
