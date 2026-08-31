//! Everything that speaks the Nearscreen protocol over TCP.
//!
//! [`Server`] listens, admits one phone at a time and reports what arrives as
//! [`ServerEvent`]s; the rest of the program never touches a socket.

pub mod discovery;
pub mod protocol;
mod server;
mod session;

pub use discovery::{local_addresses, Advertisement, Interfaces};
pub use protocol::{Codec, Hello, HelloReply, Params, StreamConfig, DEFAULT_PORT};
pub use server::{
    hostname, Admission, AdmissionFn, AllowAll, Decision, Server, ServerEvent, ServerOptions,
    SessionHandle,
};
