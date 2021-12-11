use std::{collections::BTreeMap, path::PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct Command {
    cmd: String,
    args: Vec<String>,
    env: Option<BTreeMap<String, String>>,
    workdir: Option<PathBuf>,
}

impl Command {
    pub fn new(cmd: impl ToString) -> Command {
        Command {
            cmd: cmd.to_string(),
            args: Vec::default(),
            env: None,
            workdir: None,
        }
    }

    pub fn arg(mut self, arg: impl ToString) -> Self {
        self.args.push(arg.to_string());
        self
    }

    pub fn env(mut self, field: impl ToString, value: impl ToString) -> Self {
        let env = match &mut self.env {
            Some(s) => s,
            None => {
                self.env = Some(BTreeMap::default());
                self.env.as_mut().unwrap()
            }
        };

        env.insert(field.to_string(), value.to_string());
        self
    }

    pub fn workdir(mut self, path: impl Into<PathBuf>) -> Self {
        self.workdir = Some(path.into());
        self
    }

    pub(crate) fn into_process(self) -> async_process::Command {
        let mut cmd = async_process::Command::new(self.cmd);

        for arg in self.args {
            cmd.arg(arg);
        }

        if let Some(env) = self.env {
            for (k, v) in env {
                cmd.env(k, v);
            }
        }

        if let Some(wd) = self.workdir {
            cmd.current_dir(wd);
        }

        cmd
    }
}
