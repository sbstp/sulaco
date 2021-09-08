use std::collections::BTreeMap;

use tokio::io::{AsyncBufReadExt, AsyncRead};
use tokio::process;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;

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

pub struct ProcessSpec {
    pub cmd: String,
    pub args: Vec<String>,
}

impl ProcessSpec {
    async fn stream_stdio(tag: &str, reader: impl AsyncRead + Unpin, stream: StdStream) {
        let br = tokio::io::BufReader::new(reader);
        let mut lines = br.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            match stream {
                StdStream::Stdout => println!("[{}] {}", tag, line),
                StdStream::Stderr => eprintln!("[{}] {}", tag, line),
            }
        }
    }

    pub fn spawn<T>(&self, token: T) -> anyhow::Result<RunningProcess<T>> {
        let mut child = tokio::process::Command::new(&self.cmd)
            .args(&self.args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(false)
            .spawn()?;

        let stdout = child.stdout.take().expect("stdout not piped");
        let stderr = child.stderr.take().expect("stderr not piped");

        tokio::spawn(Self::stream_stdio("echo", stdout, StdStream::Stdout));
        tokio::spawn(Self::stream_stdio("echo", stderr, StdStream::Stderr));

        let pid = child.id().expect("child has no pid");
        Ok(RunningProcess {
            handle: child,
            pid,
            token,
        })
    }
}

pub struct RunningProcess<T> {
    pub handle: process::Child,
    pub pid: u32,
    pub token: T,
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
        loop {
            let mut status = 0;
            let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
            if pid <= 0 {
                break;
            } else {
                let mut term = None;

                if libc::WIFEXITED(status) {
                    let code = libc::WEXITSTATUS(status);
                    term = Some(Termination::Exited {
                        pid: pid as u32,
                        code,
                    })
                }
                if libc::WIFSIGNALED(status) {
                    let signal = libc::WTERMSIG(status);
                    term = Some(Termination::Signaled {
                        pid: pid as u32,
                        signal,
                    })
                }

                if let Some(term) = term {
                    if sender.send(term).await.is_err() {
                        return;
                    }
                }
            }
        }
    }
}

pub struct AppState {
    specs: Vec<ProcessSpec>,
    ptable: BTreeMap<u32, RunningProcess<usize>>,
    shutdown: bool,
}

impl AppState {
    pub fn init(&mut self) {
        for (token, spec) in self.specs.iter().enumerate() {
            match spec.spawn(token) {
                Ok(proc) => {
                    self.ptable.insert(proc.pid, proc);
                }
                Err(err) => {
                    eprintln!("[~sulaco] could not spawn process '{}': {}", spec.cmd, err);
                }
            }
        }
    }

    pub fn shutdown(&mut self) {
        self.shutdown = true;
    }

    pub async fn terminated(&mut self, term: Termination) {
        if let Some(proc) = self.ptable.remove(&term.pid()) {
            if !self.shutdown {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                let spec = &self.specs[proc.token];
                let proc = spec.spawn(proc.token).unwrap();
                self.ptable.insert(proc.pid, proc);
            }
        }
    }

    pub fn should_exit(&self) -> bool {
        self.shutdown && self.ptable.is_empty()
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let (sendr, mut recvr) = tokio::sync::mpsc::channel(8);
    tokio::spawn(listen_sigchld(sendr));

    let mut sigint = signal(SignalKind::interrupt()).unwrap();
    let mut sigterm = signal(SignalKind::terminate()).unwrap();

    let spec = ProcessSpec {
        cmd: "echo".into(),
        args: vec!["hello world!".into()],
    };

    let mut state = AppState {
        specs: vec![spec],
        ptable: BTreeMap::new(),
        shutdown: false,
    };

    state.init();

    loop {
        tokio::select! {
            _ = sigint.recv() => {
                println!("[~sulaco] SIGINT received, shutting down");
                state.shutdown();
            }
            _ = sigterm.recv() => {
                println!("[~sulaco] SIGTERM received, shutting down");
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
