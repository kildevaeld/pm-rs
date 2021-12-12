mod command;
mod error;
mod manager;

// pub use signal_child::{self, signal::Signal};
pub use nix::sys::signal::Signal;

pub use self::{command::*, error::*, manager::*};
