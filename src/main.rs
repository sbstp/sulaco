use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

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

#[derive(Debug)]
pub enum Event {
    TermSignal,
    ServiceTerminated(Termination),
    ServiceForceShutdown(u32),
}

pub enum StdStream {
    Stdout,
    Stderr,
}

pub struct RunningService {
    pub name: Arc<String>,
    pub handle: process::Child,
    pub pid: u32,
    sender: mpsc::Sender<Event>,
}

impl RunningService {
    pub fn spawn(
        name: &Arc<String>,
        spec: &spec::ServiceSpec,
        sender: mpsc::Sender<Event>,
    ) -> anyhow::Result<RunningService> {
        let mut cmd = tokio::process::Command::new(&spec.cmd);
        cmd.args(&spec.args);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        unsafe {
            cmd.pre_exec(|| {
                let _ = nix::unistd::setpgid(nix::unistd::Pid::from_raw(0), nix::unistd::Pid::from_raw(0));
                Ok(())
            });
        };

        let mut child = cmd.spawn()?;

        let stdout = child.stdout.take().expect("stdout not piped");
        let stderr = child.stderr.take().expect("stderr not piped");

        tokio::spawn(Self::stream_stdio(name.clone(), stdout, StdStream::Stdout));
        tokio::spawn(Self::stream_stdio(name.clone(), stderr, StdStream::Stderr));

        let pid = child.id().expect("child has no pid");
        Ok(RunningService {
            name: name.clone(),
            handle: child,
            pid,
            sender,
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

    pub fn terminate(&self, timeout: Duration) {
        eprintln!("[~sulaco] sending SIGTERM to {} (pid={})", self.name, self.pid);
        //let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(self.pid as i32), nix::sys::signal::SIGTERM);

        let sender = self.sender.clone();
        let pid = self.pid;
        tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            let _ = sender.send(Event::ServiceForceShutdown(pid)).await;
        });
    }

    pub fn force_terminate(&self) {
        eprintln!("[~sulaco] sending SIGKILL to {} (pid={})", self.name, self.pid);
        let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(self.pid as i32), nix::sys::signal::SIGKILL);
    }
}

pub struct AppState {
    conf: spec::ConfigFile,
    sender: mpsc::Sender<Event>,
    ptable: BTreeMap<u32, RunningService>,
    shutdown: bool,
}

impl AppState {
    pub fn init(&mut self) {
        for (name, spec) in self.conf.services.iter() {
            match RunningService::spawn(name, spec, self.sender.clone()) {
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
                let proc = RunningService::spawn(&proc.name, spec, self.sender.clone()).unwrap();
                self.ptable.insert(proc.pid, proc);
            }
        }
    }

    pub fn force_terminate(&self, pid: u32) {
        self.ptable[&pid].force_terminate();
    }

    pub fn shutdown(&mut self) {
        self.shutdown = true;
        for service in self.ptable.values() {
            service.terminate(Duration::from_secs(5));
        }
    }

    pub fn should_exit(&self) -> bool {
        self.shutdown && self.ptable.is_empty()
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let (sender, mut receiver) = tokio::sync::mpsc::channel(32);
    tokio::spawn(listen_sigchld(sender.clone()));
    tokio::spawn(handle_signal(sender.clone(), SignalKind::interrupt()));
    tokio::spawn(handle_signal(sender.clone(), SignalKind::terminate()));

    let conf = spec::ConfigFile::load("sulaco.yaml").context("could not load config file")?;

    let mut state = AppState {
        conf,
        sender,
        ptable: BTreeMap::new(),
        shutdown: false,
    };

    state.init();

    while !state.should_exit() {
        let event = receiver.recv().await.expect("event channel closed unexpectedly");

        match event {
            Event::TermSignal => {
                eprintln!("[~sulaco] term signal received, shutting down...");
                state.shutdown();
            }
            Event::ServiceTerminated(term) => {
                eprintln!("[~sulaco] service terminated {:?}", term.pid());
                state.terminated(term).await;
            }
            Event::ServiceForceShutdown(pid) => {
                state.force_terminate(pid);
            }
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
async fn listen_sigchld(sender: mpsc::Sender<Event>) {
    let mut signals = signal(SignalKind::child()).expect("could not create SIGCHLD signal handler");

    while signals.recv().await.is_some() {
        // WNOHANG makes the call to waitpid nonblocking. When we receive a
        // SIGCHLD signal, we call waitpid multiple times because it's possible
        // that multiple children have exited for a single signal reception.
        // We stop looking for exited children when waitpid returns an error
        // or 0, signaling it would hang.
        while let Ok(status) = nix::sys::wait::waitpid(None, Some(nix::sys::wait::WaitPidFlag::WNOHANG)) {
            let term = match status {
                nix::sys::wait::WaitStatus::Exited(pid, code) => Some(Termination::Exited {
                    pid: pid.as_raw() as u32,
                    code,
                }),
                nix::sys::wait::WaitStatus::Signaled(pid, signal, _) => Some(Termination::Signaled {
                    pid: pid.as_raw() as u32,
                    signal: signal as i32,
                }),
                _ => None,
            };

            if let Some(term) = term {
                if sender.send(Event::ServiceTerminated(term)).await.is_err() {
                    return;
                }
            }
        }
    }
}

async fn handle_signal(sender: mpsc::Sender<Event>, sig: SignalKind) {
    let mut signals = signal(sig).unwrap();
    while let Some(_) = signals.recv().await {
        if sender.send(Event::TermSignal).await.is_err() {
            break;
        }
    }
}
