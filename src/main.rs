use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::io::{AsyncBufReadExt, AsyncRead};
use tokio::process;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;

use unix::Termination;

mod spec;
mod unix;

#[derive(Debug)]
pub enum Event {
    TermSignal,
    ServiceRestart { name: Arc<String> },
    ServiceTerminated(Termination),
    ServiceForceShutdown { pid: u32, instance_id: u64 },
}

pub enum StdStream {
    Stdout,
    Stderr,
}

pub struct RunningService {
    pub name: Arc<String>,
    pub handle: process::Child,
    pub pid: u32,
    pub instance_id: u64,
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
                unix::new_process_group();
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
            instance_id: Self::next_instance_id(),
            sender,
        })
    }

    fn next_instance_id() -> u64 {
        static INSTANCE_ID_COUNTER: AtomicU64 = AtomicU64::new(0);
        INSTANCE_ID_COUNTER.fetch_add(1, Ordering::SeqCst)
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

    pub fn terminate(&self, timeout: Duration, stop_signal: unix::Signal) {
        eprintln!(
            "[~sulaco] sending {} to [{}] (pid={})",
            stop_signal, self.name, self.pid
        );
        unix::kill(self.pid, stop_signal);

        let sender = self.sender.clone();
        let pid = self.pid;
        let instance_id = self.instance_id;
        tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            let _ = sender.send(Event::ServiceForceShutdown { pid, instance_id }).await;
        });
    }

    pub fn force_terminate(&self) {
        eprintln!("[~sulaco] sending SIGKILL to [{}] (pid={})", self.name, self.pid);
        unix::kill(self.pid, unix::Signal::Sigkill);
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

    pub fn start_service(&mut self, name: &Arc<String>) {
        let spec = &self.conf.services[name];
        let proc = RunningService::spawn(name, spec, self.sender.clone()).unwrap();
        eprintln!("[~sulaco] service started {} (pid={})", proc.name, proc.pid);
        self.ptable.insert(proc.pid, proc);
    }

    pub async fn terminated(&mut self, term: Termination) {
        if let Some(proc) = self.ptable.remove(&term.pid()) {
            eprintln!("[~sulaco] service [{}] (pid={}) terminated", proc.name, term.pid());
            let spec = &self.conf.services[&proc.name];
            if !self.shutdown {
                match spec.on_exit {
                    spec::OnExit::Restart { restart_delay } => {
                        let sender = self.sender.clone();
                        let name = proc.name.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_secs(restart_delay)).await;
                            let _ = sender.send(Event::ServiceRestart { name }).await;
                        });
                    }
                    spec::OnExit::Shutdown => {
                        eprintln!("[~sulaco] service [{}] configured to shutdown container ", proc.name);
                        self.shutdown();
                    }
                }
            }
        } else {
            eprintln!("[~sulaco] zombie reaped (pid={})", term.pid());
        }
    }

    pub fn force_terminate(&self, pid: u32, instance_id: u64) {
        let proc = &self.ptable[&pid];
        if proc.instance_id == instance_id {
            proc.force_terminate();
        }
    }

    pub fn shutdown(&mut self) {
        self.shutdown = true;
        for proc in self.ptable.values() {
            let spec = &self.conf.services[&proc.name];
            proc.terminate(spec.stop_timeout, spec.stop_signal);
        }
    }

    pub fn should_exit(&self) -> bool {
        self.shutdown && self.ptable.is_empty()
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    let (sender, mut receiver) = tokio::sync::mpsc::channel(32);
    tokio::spawn(listen_sigchld(sender.clone()));
    tokio::spawn(handle_signal(sender.clone(), SignalKind::interrupt()));
    tokio::spawn(handle_signal(sender.clone(), SignalKind::terminate()));

    let conf = spec::ConfigFile::load(&args[1]).context("could not load config file")?;

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
            Event::ServiceRestart { name } => {
                state.start_service(&name);
            }
            Event::ServiceTerminated(term) => {
                state.terminated(term).await;
            }
            Event::ServiceForceShutdown { pid, instance_id } => {
                state.force_terminate(pid, instance_id);
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
        loop {
            match unix::wait_termination() {
                unix::WaitTermination::Termination(term) => {
                    if sender.send(Event::ServiceTerminated(term)).await.is_err() {
                        return;
                    }
                }
                unix::WaitTermination::Retry => continue,
                unix::WaitTermination::Stop => break,
            }
        }
    }
}

async fn handle_signal(sender: mpsc::Sender<Event>, sig: SignalKind) {
    let mut signals = signal(sig).unwrap();
    while signals.recv().await.is_some() {
        if sender.send(Event::TermSignal).await.is_err() {
            break;
        }
    }
}
