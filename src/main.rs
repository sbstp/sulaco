use tokio::io::AsyncBufReadExt;
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

async fn listen_sigchld(sender: mpsc::Sender<Termination>) {
    let mut signals = signal(SignalKind::child()).expect("could not create SIGCHLD signal handler");

    while signals.recv().await.is_some() {
        let mut status = 0;
        let pid = unsafe { libc::waitpid(-1, &mut status, 0) };
        if pid < 0 {
            eprintln!("waitpid error: {:?}", std::io::Error::last_os_error());
        } else {
            let term = if libc::WIFEXITED(status) {
                let code = libc::WEXITSTATUS(status);
                Termination::Exited {
                    pid: pid as u32,
                    code,
                }
            } else if libc::WIFSIGNALED(status) {
                let signal = libc::WTERMSIG(status);
                Termination::Signaled {
                    pid: pid as u32,
                    signal,
                }
            } else {
                continue;
            };

            if sender.send(term).await.is_err() {
                break;
            }
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (sendr, mut recvr) = tokio::sync::mpsc::channel(8);
    tokio::spawn(listen_sigchld(sendr));

    let mut child = tokio::process::Command::new("echo")
        .arg("--wtfedef")
        .arg("hello")
        .stdout(std::process::Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().unwrap();

    tokio::spawn(async move {
        let br = tokio::io::BufReader::new(stdout);
        let mut lines = br.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            println!("[out] {}", line);
        }
    });

    while let Some(term) = recvr.recv().await {
        println!("[done] {:?}", term);
        if term.pid() == child.id().unwrap() {
            break;
        }
    }

    Ok(())
}
