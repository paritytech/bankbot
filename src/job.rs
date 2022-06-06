use std::sync::{Arc, Mutex};
use git2::build::{CheckoutBuilder, RepoBuilder};
use octocrab::models::issues::Issue;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;
use crate::api;

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
    pub fn checkout<R: AsRef<Path> + Copy>(&self, root: R) -> Result<CheckedoutJob, Error>
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
            root: PathBuf::from(root),
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
    root: PathBuf,
    repository: Repository,
    issue: Issue,
}

impl CheckedoutJob {
    fn prepare_engine(&self) -> Result<rhai::Engine, Error> {
        let mut engine = rhai::Engine::new();

        engine
            .register_type::<api::cargo::CargoResult>()
            .register_fn("is_ok", api::cargo::CargoResult::is_ok)
            .register_get("stdout", api::cargo::CargoResult::get_stdout)
            .register_get("stderr", api::cargo::CargoResult::get_stderr);

        let cargo_dir = self.dir.clone();
        engine.register_custom_syntax(&["cargo", "$expr$"], false, move |context, inputs| {
            let expr = &inputs[0];
            let value = context
                .eval_expression_tree(expr)?
                .try_cast::<String>()
                .ok_or("Failed to parse `cargo` arguments into a string")?;

            let value =
                shell_words::split(&value).map_err(|_| "Failed to parse `cargo` arguments")?;
            let cargo = api::cargo::Run::new(value, &cargo_dir);
            let result = cargo.run();
            Ok(rhai::Dynamic::from(result))
        })?;

        engine
            .register_type::<api::Issue>()
            .register_result_fn("comment", api::Issue::create_comment::<String>)
            .register_result_fn("comment", api::Issue::create_comment::<&str>)
            .register_result_fn(
                "comment",
                api::Issue::create_comment::<rhai::ImmutableString>,
            );

        engine.register_type::<api::git::Git>()
            .register_result_fn("clone", api::git::Git::clone::<String>)
            .register_result_fn("clone", api::git::Git::clone::<&str>)
            .register_result_fn("clone", api::git::Git::clone::<rhai::ImmutableString>)
        ;

        //let git = exported_module!(api::git::Git);

        engine.register_type::<api::git::LocalRepo>()
            .register_result_fn("read", api::git::LocalRepo::read_file::<PathBuf>)
            .register_result_fn("read", api::git::LocalRepo::read_file::<api::git::DirEntryPath>)
            .register_result_fn("read", api::git::LocalRepo::read_file::<&Path>)
            .register_result_fn("read", api::git::LocalRepo::read_file::<String>)
            .register_result_fn("read", api::git::LocalRepo::read_file::<&str>)
            .register_result_fn("write", api::git::LocalRepo::write_file::<PathBuf>)
            .register_result_fn("write", api::git::LocalRepo::write_file::<api::git::DirEntryPath>)
            /*
            .register_result_fn("write", api::git::LocalRepo::write_file::<PathBuf, &[u8]>)
            .register_result_fn("write", api::git::LocalRepo::write_file::<&Path, &[u8]>)
            .register_result_fn("write", api::git::LocalRepo::write_file::<String, &[u8]>)
            .register_result_fn("write", api::git::LocalRepo::write_file::<&str, &[u8]>)
            */
            .register_result_fn("ls", api::git::LocalRepo::list_files)
            .register_result_fn("ls", api::git::LocalRepo::list_files_in_dir::<PathBuf>)
            .register_result_fn("ls", api::git::LocalRepo::list_files_in_dir::<&Path>)
            .register_result_fn("ls", api::git::LocalRepo::list_files_in_dir::<String>)
            .register_result_fn("ls", api::git::LocalRepo::list_files_in_dir::<&str>)
            /*
            .register_result_fn("add", api::git::LocalRepo::add::<PathBuf>)
            .register_result_fn("add", api::git::LocalRepo::add::<String>)
            .register_result_fn("add", api::git::LocalRepo::add::<&Path>)
            .register_result_fn("add", api::git::LocalRepo::add::<&str>)
            */
            .register_result_fn("add", api::git::LocalRepo::add::<api::git::DirEntryPath>)
            .register_result_fn("ls-modified", api::git::LocalRepo::list_modified)
            .register_result_fn("status", api::git::LocalRepo::pub_status)
            .register_result_fn("commit", api::git::LocalRepo::pub_commit::<String>)
            .register_result_fn("push", api::git::LocalRepo::pub_push::<String>)
            .register_result_fn("push", api::git::LocalRepo::pub_push::<&str>)
            .register_result_fn("push", api::git::LocalRepo::pub_push::<rhai::ImmutableString>)
            ;

        engine.register_type::<api::git::DirEntry>()
            .register_get("path", api::git::DirEntry::get_path)
            .register_fn("is_file", api::git::DirEntry::is_file)
            .register_fn("is_dir", api::git::DirEntry::is_dir)
            .register_fn("is_symlink", api::git::DirEntry::is_symlink)
            ;

        engine.register_type::<api::git::Status>()
            .register_result_fn("changed", api::git::Status::pub_changed)
            .register_result_fn("added", api::git::Status::pub_added)
            .register_result_fn("deleted", api::git::Status::pub_deleted)
            ;

        engine.register_type::<api::git::DirEntryPath>()
            .register_fn("strip_prefix", api::git::DirEntryPath::strip_prefix::<PathBuf>)
            .register_fn("strip_prefix", api::git::DirEntryPath::strip_prefix::<&Path>)
            .register_fn("strip_prefix", api::git::DirEntryPath::strip_prefix::<String>)
            .register_fn("strip_prefix", api::git::DirEntryPath::strip_prefix::<&str>)
            ;

        Ok(engine)
    }

    fn script_path(&self) -> Result<PathBuf, Error> {
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
        Ok(Path::new(".github").join(dir).join(file))
    }

    pub fn prepare_script(
        self,
        github_client: octocrab::Octocrab,
        tokio_handle: tokio::runtime::Handle,
    ) -> Result<RunnableJob<'static>, Error> {
        log::debug!("Preparing script");
        let script_path = self.script_path()?;

        let engine = self.prepare_engine()?;

        let client = Arc::new(Mutex::new(github_client));

        let scope = {
            let mut scope = rhai::Scope::new();
            let repo_name = self.repository.name.clone();
            let issue = api::Issue::new(client.clone(), self.repository, self.issue);
            scope.push_constant("issue", issue);
            let local_repo = git2::Repository::open(&self.dir)?;
            let repo = api::git::LocalRepo::new(&self.dir, repo_name, local_repo, client.clone(), tokio_handle.clone());
            scope.push_constant("repo", repo);
            // TODO: replace with proper module export
            let git = api::git::Git{path: self.dir.clone(), root: self.root, github_client: client, tokio_handle};
            scope.push_constant("Git", git);
            Box::new(scope)
        };

        Ok(RunnableJob {
            //job: self.job,
            dir: self.dir,
            script_path,
            engine,
            scope,
        })
    }
}

pub struct RunnableJob<'a> {
    dir: PathBuf,
    script_path: PathBuf,
    engine: rhai::Engine,
    scope: Box<rhai::Scope<'a>>,
}

impl RunnableJob<'_> {
    pub fn run(mut self) -> Result<(), Error> {
        log::info!(
            "Executing {} in {:?}",
            self.script_path.to_string_lossy(),
            self.dir
        );

        // We don't want to leak any internal fs details
        let ast = self.engine.compile_file(self.dir.join(self.script_path.clone()))
            // Don't leak in the internal path
            .map_err(|e| Error::ScriptExecution(format!("{e}").replace(&*self.dir.to_string_lossy(), ".").into()))
            ?;

        self.engine.run_ast_with_scope(&mut self.scope, &ast)?;
        Ok(())
    }
}
