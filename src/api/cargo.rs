use std::path::{Path, PathBuf};

pub struct Run {
    args: Vec<String>,
    dir: PathBuf,
}

impl Run {
    pub fn new<S: ToString, A: AsRef<[S]>, P: AsRef<Path>>(args: A, dir: P) -> Self {
        let args = args.as_ref().iter().map(|arg| arg.to_string()).collect();
        let dir = dir.as_ref().into();
        Run { args, dir }
    }

    pub fn run(self) -> CargoResult {
        log::info!("Running cargo in {:?} with args {:?}", self.dir, self.args);
        match std::process::Command::new("cargo")
            .env_clear()
            .stdin(std::process::Stdio::null())
            .args(self.args)
            .output()
        {
            Ok(output) => CargoResult {
                exit_code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            },
            Err(e) => CargoResult {
                exit_code: Some(-1),
                stdout: "".into(),
                stderr: format!("Error executing cargo: {}", e),
            },
        }
    }
}

#[derive(Clone, Debug)]
pub struct CargoResult {
    pub exit_code: Option<i32>, // remove `pub` after mocking
    pub stdout: String,
    pub stderr: String,
}

impl CargoResult {
    // The &mut self is required by
    // [rhai](https://rhai.rs/book/rust/custom.html#first-parameter-must-be-mut).
    #[allow(clippy::wrong_self_convention)]
    pub fn is_ok(&mut self) -> bool {
        self.exit_code == Some(0)
    }

    pub fn get_stderr(&mut self) -> String {
        self.stderr.clone()
    }

    pub fn get_stdout(&mut self) -> String {
        self.stdout.clone()
    }
}
