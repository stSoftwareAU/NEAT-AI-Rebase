//! Opt-in replay experiment support, shared with its behavioural tests.

mod manifest;
mod results;
mod runner;
pub(crate) mod scorer;
#[cfg(not(test))]
pub(crate) mod smoke;

pub(crate) use manifest::freeze;
pub(crate) use runner::run;
