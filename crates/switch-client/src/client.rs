//! The public entry point: `Client::connect(...)` returns an
//! `mpsc::Receiver<Event>` of domain events (with any pending gap attached)
//! and a cloneable `Sender` handle for posting messages and calling
//! operations bound to this connection.

// written by hand, do not generate
