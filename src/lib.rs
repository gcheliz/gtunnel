//! gtunnel as a library, so tests can reach the modules without spawning the
//! binary. Everything is crate-private except what `main` and the tests
//! genuinely call: widening a module here is a decision, not a default.

mod breaker;
pub mod cli;
mod commands;
mod config;
mod daemon;
mod discovery;
mod doctor;
mod ephemeral;
mod error;
mod expose;
mod fields;
mod fsutil;
mod gcp;
mod http;
mod lifecycle;
mod logfilter;
mod loglevel;
pub mod mcp;
mod notify;
mod probe;
mod proc;
mod restart_budget;
mod settings;
mod state;
mod tui;
mod tunnel;
mod update;

pub mod dispatch;
