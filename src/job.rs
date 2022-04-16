use git2::build::{CheckoutBuilder, RepoBuilder};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Failed to clone repository: {source}")]
    Clone {
        #[from]
        source: git2::Error,
    },
    #[error("No benchmark job scripts found")]
    NoScriptFound(#[from] std::io::Error),
    #[error("Failed to find a URL to clone the repository")]
    NoCloneUrl,
    #[error("Missing bot command")]
    NoCmd,
    #[error("Failed to checkout repository because path {0} exists but is not a directory")]
    NoDirectory(PathBuf),
    #[error("Failed to execute script")]
    ScriptExecution(#[from] Box<rhai::EvalAltResult>),
    #[error("Failed to parse script")]
    ScriptParse(#[from] rhai::ParseError),
    #[error("Failed to parse cargo command")]
    CargoCmdParse,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Job {
    pub command: String,
    pub user: octocrab::models::User,
    pub repository: octocrab::models::Repository,
    pub issue: octocrab::models::issues::Issue,
}

impl Job {
    fn pr_branch(&self) -> String {
        format!("pull/{}/head", self.issue.number)
    }

    // This function assumes at most one Job::checkout() run at any time. This requirement is
    // because of FS mutation, which unfortunately the type checker can't help us with. Currently
    // this is guaranteed by spawning only one thread that synchronously runs jobs.
    pub fn checkout<R: AsRef<Path>>(&self, root: R) -> Result<CheckedoutJob, Error>
    where
        PathBuf: From<R>,
    {
        let dir = self.repo_dir(root);
        let branch = self.pr_branch();
        let repo = match std::fs::metadata(&dir) {
            Ok(metadata) if metadata.is_dir() => git2::Repository::open(&dir)?,
            Err(_) => {
                // Path doesn't exist
                let url = self
                    .repository
                    .clone_url
                    .as_ref()
                    .ok_or(Error::NoCloneUrl)?;

                let mut checkout = CheckoutBuilder::new();
                checkout.remove_untracked(true).remove_ignored(true).force();
                log::info!("Cloning {} to {:?}", url, &dir);
                RepoBuilder::new()
                    .with_checkout(checkout)
                    .clone(url.as_ref(), &dir)?
            }
            Ok(_) => {
                log::warn!("Path {:?} exists but is not a directory", dir);
                return Err(Error::NoDirectory(dir));
            }
        };

        log::info!("Fetching {} in {:?}", branch, dir);
        repo.find_remote("origin")?.fetch(
            &[&format!("refs/{}:refs/heads/{}", branch, branch)],
            None,
            None,
        )?;

        let rev = repo.revparse_single("FETCH_HEAD")?;
        repo.reset(
            &rev,
            git2::ResetType::Hard,
            Some(
                CheckoutBuilder::new()
                    .remove_untracked(true)
                    .remove_ignored(true)
                    .force(),
            ),
        )?;

        let job = CheckedoutJob {
            job: self.clone(),
            dir,
        };
        Ok(job)
    }

    fn repo_dir<R: AsRef<Path>>(&self, root: R) -> PathBuf
    where
        PathBuf: From<R>,
    {
        let mut full_path = PathBuf::from(root);

        let mut dir_name = format!(
            "{}_{}_{}_",
            self.repository.id, self.issue.id, self.user.login
        );
        if let Some(owner) = &self.repository.owner {
            dir_name.push_str(&owner.login);
            dir_name.push('_');
        }
        dir_name.push_str(&self.repository.name);
        full_path.set_file_name(dir_name);
        full_path
    }
}

pub struct CheckedoutJob {
    job: Job,
    dir: PathBuf,
}

impl CheckedoutJob {
    pub fn prepare_script(self) -> Result<RunnableJob, Error> {
        let script = {
            let dir = self
                .job
                .command
                .split(' ')
                .next()
                .map(|cmd| {
                    if let Some(cmd) = cmd.strip_prefix('/') {
                        cmd
                    } else {
                        cmd
                    }
                })
                .ok_or(Error::NoCmd)?;
            let file = self
                .job
                .command
                .split(' ')
                .nth(1)
                .map(|cmd| format!("{}.rhai", cmd))
                .ok_or(Error::NoCmd)?;
            let rel_script_path = Path::new(".github").join(dir).join(file);
            self.dir.join(&rel_script_path)
        };

        let cargo_dir = self.dir.clone();
        let engine = {
            let mut engine = rhai::Engine::new();

            engine
                .register_type::<api::CargoResult>()
                .register_fn("is_ok", api::CargoResult::is_ok)
                .register_get("stdout", api::CargoResult::get_stdout)
                .register_get("stderr", api::CargoResult::get_stderr);

            engine.register_custom_syntax(
                &["cargo", "$expr$"],
                false,
                move |context, inputs| {
                    let expr = &inputs[0];
                    let value = context
                        .eval_expression_tree(expr)?
                        .try_cast::<String>()
                        .ok_or("Failed to parse `cargo` arguments into a string")?;

                    let value = shell_words::split(&value)
                        .map_err(|_| "Failed to parse `cargo` arguments")?;
                    let cargo = CargoRun::new(value, &cargo_dir);
                    let result = cargo.run();
                    Ok(rhai::Dynamic::from(result))
                },
            )?;

            engine
        };

        Ok(RunnableJob {
            job: self.job,
            dir: self.dir,
            script,
            engine,
        })
    }
}

struct CargoRun {
    args: Vec<String>,
    dir: PathBuf,
}

impl CargoRun {
    fn new<S: ToString, A: AsRef<[S]>, P: AsRef<Path>>(args: A, dir: P) -> Self {
        let args = args.as_ref().iter().map(|arg| arg.to_string()).collect();
        let dir = dir.as_ref().into();
        CargoRun { args, dir }
    }

    fn run(self) -> api::CargoResult {
        log::info!("Running cargo in {:?} with args {:?}", self.dir, self.args);
        match std::process::Command::new("cargo")
            .env_clear()
            .stdin(std::process::Stdio::null())
            .args(self.args)
            .output()
        {
            Ok(output) => api::CargoResult {
                exit_code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            },
            Err(e) => api::CargoResult {
                exit_code: Some(-1),
                stdout: "".into(),
                stderr: format!("Error executing cargo: {}", e),
            },
        }
    }
}

mod api {
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
            log::info!("exit code: {:?}", self.exit_code);
            self.exit_code == Some(0)
        }

        pub fn get_stderr(&mut self) -> String {
            self.stderr.clone()
        }

        pub fn get_stdout(&mut self) -> String {
            self.stdout.clone()
        }
    }
}

pub struct RunnableJob {
    job: Job,
    dir: PathBuf,
    script: PathBuf,
    engine: rhai::Engine,
}

impl RunnableJob {
    pub fn run(self) -> Result<(), Error> {
        log::info!(
            "Executing {:?} in {:?}",
            self.script.strip_prefix(&self.dir),
            self.dir
        );

        match self.engine.eval_file::<String>(self.script) {
            Ok(e) => log::info!("Script result: {}", e),
            Err(e) => log::warn!("Failed to execute script: {:?}", e),
        };

        Ok(())
    }
}
