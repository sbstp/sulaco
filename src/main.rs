use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Context;
use tokio::io::{AsyncBufReadExt, AsyncRead};
use tokio::process;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;

mod spec;

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

pub enum StdStream {
    Stdout,
    Stderr,
}

pub struct RunningService {
    pub name: Arc<String>,
    pub handle: process::Child,
    pub pid: u32,
}

impl RunningService {
    pub fn spawn(name: &Arc<String>, spec: &spec::ServiceSpec) -> anyhow::Result<RunningService> {
        let mut child = tokio::process::Command::new(&spec.cmd)
            .args(&spec.args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(false)
            .spawn()?;

        let stdout = child.stdout.take().expect("stdout not piped");
        let stderr = child.stderr.take().expect("stderr not piped");

        tokio::spawn(Self::stream_stdio(name.clone(), stdout, StdStream::Stdout));
        tokio::spawn(Self::stream_stdio(name.clone(), stderr, StdStream::Stderr));

        let pid = child.id().expect("child has no pid");
        Ok(RunningService {
            name: name.clone(),
            handle: child,
            pid,
        })
    }

    async fn stream_stdio(tag: Arc<String>, reader: impl AsyncRead + Unpin, stream: StdStream) {
        let br = tokio::io::BufReader::new(reader);
        let mut lines = br.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            match stream {
                StdStream::Stdout => println!("[{}] {}", tag, line),
                StdStream::Stderr => eprintln!("[{}] {}", tag, line),
            }
        }
    }

    pub fn terminate(&self) {
        eprintln!(
            "[~sulaco] sending SIGTERM to {} (pid={})",
            self.name, self.pid
        );
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(self.pid as i32),
            nix::sys::signal::SIGTERM,
        );
    }
}

pub struct AppState {
    conf: spec::ConfigFile,
    ptable: BTreeMap<u32, RunningService>,
    shutdown: bool,
}

impl AppState {
    pub fn init(&mut self) {
        for (name, spec) in self.conf.services.iter() {
            match RunningService::spawn(name, spec) {
                Ok(proc) => {
                    self.ptable.insert(proc.pid, proc);
                }
                Err(err) => {
                    eprintln!("[~sulaco] could not spawn process '{}': {}", spec.cmd, err);
                }
            }
        }
    }

    pub async fn terminated(&mut self, term: Termination) {
        if let Some(proc) = self.ptable.remove(&term.pid()) {
            let spec = &self.conf.services[&proc.name];
            if !self.shutdown {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                let proc = RunningService::spawn(&proc.name, spec).unwrap();
                self.ptable.insert(proc.pid, proc);
            }
        }
    }

    pub fn shutdown(&mut self) {
        self.shutdown = true;
        for service in self.ptable.values() {
            service.terminate();
        }
    }

    pub fn should_exit(&self) -> bool {
        self.shutdown && self.ptable.is_empty()
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let conf = spec::ConfigFile::load("sulaco.yaml").context("could not load config file")?;

    let (sendr, mut recvr) = tokio::sync::mpsc::channel(32);
    tokio::spawn(listen_sigchld(sendr));

    let mut sigint = signal(SignalKind::interrupt()).unwrap();
    let mut sigterm = signal(SignalKind::terminate()).unwrap();

    let mut state = AppState {
        conf,
        ptable: BTreeMap::new(),
        shutdown: false,
    };

    state.init();

    loop {
        tokio::select! {
            _ = sigint.recv() => {
                println!("\n[~sulaco] SIGINT received, shutting down...");
                state.shutdown();
            }
            _ = sigterm.recv() => {
                println!("\n[~sulaco] SIGTERM received, shutting down...");
                state.shutdown();
            }
            term = recvr.recv() => {
                if let Some(term) = term {
                    println!("[~sulaco] process terminated {:?}", term.pid());
                    state.terminated(term).await;
                }
            }
        };

        if state.should_exit() {
            break;
        }
    }

    Ok(())
}

// Listen to the SIGCHLD signal and call waitpid when the signal is received.
// After that the termination event, if any, is sent through the channel.
// The waitpid function will both reap zombies and catch termination of children
// processes. Events referring to zombies can be ignored but events tied to a
// child process we know might need to be handled, for instance restarting
// a killed child process or failed child process.
async fn listen_sigchld(sender: mpsc::Sender<Termination>) {
    let mut signals = signal(SignalKind::child()).expect("could not create SIGCHLD signal handler");

    while signals.recv().await.is_some() {
        // WNOHANG makes the call to waitpid nonblocking. When we receive a
        // SIGCHLD signal, we call waitpid multiple times because it's possible
        // that multiple children have exited for a single signal reception.
        // We stop looking for exited children when waitpid returns an error
        // or 0, signaling it would hang.
        while let Ok(status) =
            nix::sys::wait::waitpid(None, Some(nix::sys::wait::WaitPidFlag::WNOHANG))
        {
            let term = match status {
                nix::sys::wait::WaitStatus::Exited(pid, code) => Some(Termination::Exited {
                    pid: pid.as_raw() as u32,
                    code,
                }),
                nix::sys::wait::WaitStatus::Signaled(pid, signal, _) => {
                    Some(Termination::Signaled {
                        pid: pid.as_raw() as u32,
                        signal: signal as i32,
                    })
                }
                _ => None,
            };

            if let Some(term) = term {
                if sender.send(term).await.is_err() {
                    return;
                }
            }
        }
    }
}
