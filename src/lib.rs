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
// Callers arrive one commit at a time (exit codes, advice, supervisor), each
// landing green; the allowance goes with the last of them
#[allow(dead_code)]
mod failure;
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
