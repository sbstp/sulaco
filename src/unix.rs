use std::fmt::Display;

use nix::sys::signal;
use nix::sys::wait;
use nix::unistd::{self, Pid};
use serde::Deserialize;

pub fn new_process_group() {
    let zero = Pid::from_raw(0);
    unistd::setpgid(zero, zero).expect("could not setpgid")
}

#[derive(Clone, Copy, Debug, Deserialize)]
pub enum Signal {
    Sigint,
    Sigterm,
    Sigkill,
}

impl Signal {
    fn to_nix(&self) -> signal::Signal {
        match self {
            Signal::Sigint => signal::SIGINT,
            Signal::Sigterm => signal::SIGTERM,
            Signal::Sigkill => signal::SIGKILL,
        }
    }
}

impl Display for Signal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Signal::Sigint => write!(f, "SIGINT"),
            Signal::Sigterm => write!(f, "SIGTERM"),
            Signal::Sigkill => write!(f, "SIGKILL"),
        }
    }
}

pub fn kill(pid: u32, signal: Signal) {
    let _ = signal::kill(Pid::from_raw(pid as i32), signal.to_nix());
}

#[derive(Debug)]
pub enum Termination {
    Exited { pid: u32, code: i32 },
    Signaled { pid: u32, signal: i32 },
}

impl Termination {
    pub fn pid(&self) -> u32 {
        match *self {
            Termination::Exited { pid, .. } => pid,
            Termination::Signaled { pid, .. } => pid,
        }
    }
}

#[derive(Debug)]
pub enum WaitTermination {
    Termination(Termination),
    Retry,
    Stop,
}

pub fn wait_termination() -> WaitTermination {
    match wait::waitpid(None, Some(wait::WaitPidFlag::WNOHANG)) {
        Ok(wait::WaitStatus::StillAlive) => WaitTermination::Stop,
        Err(_) => WaitTermination::Stop,
        Ok(status) => {
            let term = match status {
                wait::WaitStatus::Exited(pid, code) => Some(Termination::Exited {
                    pid: pid.as_raw() as u32,
                    code,
                }),
                wait::WaitStatus::Signaled(pid, signal, _) => Some(Termination::Signaled {
                    pid: pid.as_raw() as u32,
                    signal: signal as i32,
                }),
                _ => None,
            };

            match term {
                Some(term) => WaitTermination::Termination(term),
                None => WaitTermination::Retry,
            }
        }
    }
}
