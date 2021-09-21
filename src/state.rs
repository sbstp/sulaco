use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use tokio::process;
use tokio::sync::mpsc;

use crate::process::RunningProcess;
use crate::spec;
use crate::unix::Termination;
use crate::Event;

pub struct BaseState {
    conf: Arc<spec::ConfigFile>,
    sender: mpsc::Sender<Event>,
    ptable: BTreeMap<u32, RunningProcess>,
}

impl BaseState {
    pub fn new(conf: &Arc<spec::ConfigFile>, sender: &mpsc::Sender<Event>) -> BaseState {
        BaseState {
            conf: conf.clone(),
            sender: sender.clone(),
            ptable: BTreeMap::new(),
        }
    }

    pub fn owns_pid(&self, pid: u32) -> bool {
        self.ptable.contains_key(&pid)
    }

    pub fn force_terminate(&self, pid: u32, instance_id: u64) {
        if let Some(proc) = self.ptable.get(&pid) {
            if proc.instance_id == instance_id {
                proc.force_terminate();
            }
        }
    }
}

pub struct InitsState {
    base: BaseState,
    done: BTreeSet<Arc<String>>,
}

impl InitsState {
    pub fn start_all(&mut self) {
        for (name, spec) in self.base.conf.init.iter() {
            let mut cmd = process::Command::new(&spec.cmd);
            cmd.args(&spec.args);

            match RunningProcess::spawn(name, self.base.sender.clone(), cmd) {
                Ok(proc) => {
                    self.base.ptable.insert(proc.pid, proc);
                }
                Err(err) => {
                    eprintln!("[~sulaco] could not spawn init '{}': {}", spec.cmd, err);
                }
            }
        }
    }

    pub fn terminated(&mut self, term: Termination) {
        if let Some(proc) = self.base.ptable.remove(&term.pid()) {
            eprintln!("[~sulaco] init [{}] (pid={}) terminated", proc.name, term.pid());
            self.done.insert(proc.name);
        }
    }

    pub fn all_done(&self) -> bool {
        self.base.conf.init.keys().all(|name| self.done.contains(name))
    }

    pub fn owns_pid(&self, pid: u32) -> bool {
        self.base.owns_pid(pid)
    }

    pub fn force_terminate(&self, pid: u32, instance_id: u64) {
        self.base.force_terminate(pid, instance_id)
    }

    pub fn shutdown(&mut self) {
        for proc in self.base.ptable.values() {
            let spec = &self.base.conf.init[&proc.name];
            proc.terminate(spec.stop_timeout, spec.stop_signal);
        }
    }
}

pub struct ServicesState {
    base: BaseState,
    shutdown: bool,
}

impl ServicesState {
    pub fn start_all(&mut self) {
        let names: Vec<Arc<String>> = self.base.conf.services.keys().cloned().collect();
        for name in names {
            self.start(&name);
        }
    }

    pub fn start(&mut self, name: &Arc<String>) {
        let spec = &self.base.conf.services[name];
        let mut cmd = process::Command::new(&spec.cmd);
        cmd.args(&spec.args);

        match RunningProcess::spawn(name, self.base.sender.clone(), cmd) {
            Ok(proc) => {
                self.base.ptable.insert(proc.pid, proc);
            }
            Err(err) => {
                eprintln!("[~sulaco] could not spawn service '{}': {}", spec.cmd, err);
            }
        }
    }

    pub async fn terminated(&mut self, term: Termination) {
        if let Some(proc) = self.base.ptable.remove(&term.pid()) {
            eprintln!("[~sulaco] service [{}] (pid={}) terminated", proc.name, term.pid());
            let spec = &self.base.conf.services[&proc.name];
            if !self.shutdown {
                match spec.on_exit {
                    spec::OnExit::Restart { restart_delay } => {
                        let sender = self.base.sender.clone();
                        let name = proc.name.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_secs(restart_delay)).await;
                            let _ = sender.send(Event::ServiceRestart { name }).await;
                        });
                    }
                    spec::OnExit::Shutdown => {
                        eprintln!("[~sulaco] service [{}] configured to shutdown container ", proc.name);
                        let _ = self.base.sender.send(Event::Shutdown).await;
                    }
                }
            }
        }
    }

    pub fn owns_pid(&self, pid: u32) -> bool {
        self.base.owns_pid(pid)
    }

    pub fn force_terminate(&self, pid: u32, instance_id: u64) {
        self.base.force_terminate(pid, instance_id)
    }

    pub fn shutdown(&mut self) {
        self.shutdown = true;
        for proc in self.base.ptable.values() {
            let spec = &self.base.conf.services[&proc.name];
            proc.terminate(spec.stop_timeout, spec.stop_signal);
        }
    }
}

pub struct AppState {
    pub inits: InitsState,
    pub services: ServicesState,
    pub shutdown: bool,
}

impl AppState {
    pub fn new(conf: &Arc<spec::ConfigFile>, sender: &mpsc::Sender<Event>) -> AppState {
        AppState {
            inits: InitsState {
                base: BaseState::new(conf, sender),
                done: BTreeSet::new(),
            },
            services: ServicesState {
                base: BaseState::new(conf, sender),
                shutdown: false,
            },
            shutdown: false,
        }
    }

    pub fn shutdown(&mut self) {
        self.shutdown = true;
        self.inits.shutdown();
        self.services.shutdown();
    }

    pub fn should_exit(&self) -> bool {
        self.inits.base.ptable.is_empty() && self.services.base.ptable.is_empty()
    }
}
