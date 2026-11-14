//! Fabric's pure core.
//!
//! Every rule is a total function: no I/O, no clock, no random source, no
//! panics and no `unsafe`. Time and random values arrive as arguments, and
//! effects come back as data. The crate is `no_std`, so it cannot name a file,
//! a socket or a clock. The specification is `state_machines.md` (M1 to M24,
//! R0, L0, H1, H2); each module names the machine it implements.
#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod asset;
pub mod claim;
pub mod config;
pub mod detect;
pub mod hook;
pub mod ids;
pub mod link;
pub mod phi;
pub mod restrict;
pub mod seat;
pub mod sig;
pub mod snap;
pub mod time;
