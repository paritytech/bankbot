use async_std::sync::{Arc, RwLock};
use git2::build::{CheckoutBuilder, RepoBuilder};
use octocrab::models::issues::Issue;
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
    #[error("Failed to execute script: {0}")]
    ScriptExecution(#[from] Box<rhai::EvalAltResult>),
    #[error("Failed to parse script")]
    ScriptParse(#[from] rhai::ParseError),
    #[error("Failed to parse cargo command")]
    CargoCmdParse,
    #[error("Failed to parse Repository: missing field \"{0}\"")]
    MissingRepositoryField(String),
}

// We use our own `Repository` definition instead of `octocrab::models::Repository` so we can make
// some fields a `T` instead of an `Option<T>` (like `owner` and `clone_url`) since that fits the
// Github payloads we should receive and simplifies downstream code.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Repository {
    pub id: octocrab::models::RepositoryId,
    pub name: String,
    pub url: url::Url,
    pub owner: octocrab::models::User,
    clone_url: url::Url,
}

impl std::convert::TryFrom<octocrab::models::Repository> for Repository {
    type Error = Error;

    fn try_from(repo: octocrab::models::Repository) -> Result<Self, Self::Error> {
        let owner = repo
            .owner
            .ok_or_else(|| Error::MissingRepositoryField("owner".into()))?;
        let clone_url = repo
            .clone_url
            .ok_or_else(|| Error::MissingRepositoryField("clone_url".into()))?;
        Ok(Repository {
            id: repo.id,
            name: repo.name,
            url: repo.url,
            owner,
            clone_url,
        })
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Job {
    pub command: String,
    pub user: octocrab::models::User,
    pub repository: Repository,
    pub issue: Issue,
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
                let url = self.repository.clone_url.as_ref();

                let mut checkout = CheckoutBuilder::new();
                checkout.remove_untracked(true).remove_ignored(true).force();
                log::info!("Cloning {} to {:?}", &self.repository.clone_url, &dir);
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
            repository: self.repository.clone(),
            issue: self.issue.clone(),
        };
        Ok(job)
    }

    fn repo_dir<R: AsRef<Path>>(&self, root: R) -> PathBuf
    where
        PathBuf: From<R>,
    {
        let mut full_path = PathBuf::from(root);
        let dir_name = format!(
            "{}_{}_{}_{}_{}",
            self.repository.id,
            self.issue.number,
            self.user.login,
            &self.repository.owner.login,
            &self.repository.name
        );
        full_path.set_file_name(dir_name);
        full_path
    }
}

pub struct CheckedoutJob {
    job: Job,
    dir: PathBuf,
    repository: Repository,
    issue: Issue,
}

impl CheckedoutJob {
    fn prepare_engine(&self) -> Result<rhai::Engine, Error> {
        let mut engine = rhai::Engine::new();

        engine
            .register_type::<api::CargoResult>()
            .register_fn("is_ok", api::CargoResult::is_ok)
            .register_get("stdout", api::CargoResult::get_stdout)
            .register_get("stderr", api::CargoResult::get_stderr);

        let cargo_dir = self.dir.clone();
        engine.register_custom_syntax(&["cargo", "$expr$"], false, move |context, inputs| {
            let expr = &inputs[0];
            let value = context
                .eval_expression_tree(expr)?
                .try_cast::<String>()
                .ok_or("Failed to parse `cargo` arguments into a string")?;

            let value =
                shell_words::split(&value).map_err(|_| "Failed to parse `cargo` arguments")?;
            let cargo = api::CargoRun::new(value, &cargo_dir);
            let result = cargo.run();
            Ok(rhai::Dynamic::from(result))
        })?;

        engine
            .register_type::<api::Issue>()
            .register_result_fn("create_comment", api::Issue::create_comment::<String>)
            .register_result_fn("create_comment", api::Issue::create_comment::<&str>)
            .register_result_fn(
                "create_comment",
                api::Issue::create_comment::<rhai::ImmutableString>,
            );

        Ok(engine)
    }

    fn get_script_path(&self) -> Result<PathBuf, Error> {
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
        Ok(self.dir.join(&rel_script_path))
    }

    pub fn prepare_script(
        self,
        github_client: Arc<RwLock<octocrab::Octocrab>>,
    ) -> Result<RunnableJob<'static>, Error> {
        let script_path = self.get_script_path()?;

        let engine = self.prepare_engine()?;

        let client = github_client;

        let scope = {
            let mut scope = rhai::Scope::new();
            let issue = api::Issue::new(client, self.repository, self.issue);
            scope.push_constant("issue", issue);
            Box::new(scope)
        };

        Ok(RunnableJob {
            job: self.job,
            dir: self.dir,
            script_path,
            engine,
            scope,
        })
    }
}

pub struct RunnableJob<'a> {
    #[allow(unused)]
    job: Job,
    dir: PathBuf,
    script_path: PathBuf,
    engine: rhai::Engine,
    scope: Box<rhai::Scope<'a>>,
}

impl RunnableJob<'_> {
    pub fn run(mut self) -> Result<(), Error> {
        log::info!(
            "Executing {:?} in {:?}",
            self.script_path.strip_prefix(&self.dir),
            self.dir
        );

        let ast = self.engine.compile_file(self.script_path.clone())?;

        self.engine.run_ast_with_scope(&mut self.scope, &ast)?;
        Ok(())
    }
}

pub mod api {
    use async_std::sync::{Arc, RwLock};
    use std::path::{Path, PathBuf};
    use thiserror::Error;

    #[derive(Error, Debug)]
    pub enum Error {
        #[error("Failed to create comment: {0}")]
        CreateComment(#[from] octocrab::Error),
        #[error("Error calling Github API: {0}")]
        GithubApiError(String),
    }

    pub struct CargoRun {
        args: Vec<String>,
        dir: PathBuf,
    }

    impl CargoRun {
        pub fn new<S: ToString, A: AsRef<[S]>, P: AsRef<Path>>(args: A, dir: P) -> Self {
            let args = args.as_ref().iter().map(|arg| arg.to_string()).collect();
            let dir = dir.as_ref().into();
            CargoRun { args, dir }
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

    use super::Repository;
    #[derive(Clone, Debug)]
    pub struct Issue {
        client: Arc<RwLock<octocrab::Octocrab>>,
        repository: Repository,
        issue: octocrab::models::issues::Issue,
    }

    use std::convert::TryInto;

    impl Issue {

        pub fn create_comment<S: AsRef<str>>(
            &mut self,
            body: S,
        ) -> Result<octocrab::models::issues::Comment, Box<rhai::EvalAltResult>> {
            // Unfortunately (like I just found out) octocrab depends on reqwest which depends on
            // tokio. Octocrab has an issue to fix that though, which I just might do :D
            //
            // TODO: Think about ways to re-use the tokio runtime
            // TODO: Fix https://github.com/XAMPPRocky/octocrab/issues/99
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build().map_err(|e| format!("{}", e))?;

            log::debug!("about to get a list of issues");
            rt.block_on( async {
                let page = self.client
                    .read()
                    .await
                    .issues(&self.repository.owner.login, &self.repository.name)
                    .list()
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;

                log::debug!("page of issues: {:?}", page);

                self.client
                    .read()
                    .await
                    .issues(&self.repository.owner.login, &self.repository.name)
                    .create_comment(self.issue.number.try_into().map_err(|e: std::num::TryFromIntError| e.to_string())?, body)
                    .await
                    .map_err(|e| e.to_string().into())
            })
        }

        pub fn new(
            client: Arc<RwLock<octocrab::Octocrab>>,
            repository: Repository,
            issue: octocrab::models::issues::Issue,
        ) -> Self {
            Issue {
                client,
                repository,
                issue,
            }
        }
    }
}
