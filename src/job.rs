use git2::build::{CheckoutBuilder, RepoBuilder};
use serde::{Deserialize, Serialize};
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
    NoDirectory(std::path::PathBuf),
    #[error("Failed to execute script")]
    ScriptExecution(#[from] Box<rhai::EvalAltResult>),
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
    pub fn checkout<R: AsRef<std::path::Path>>(&self, root: R) -> Result<CheckedoutJob, Error>
    where
        std::path::PathBuf: From<R>,
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
                log::debug!("Cloning {} to {:?}", url, &dir);
                RepoBuilder::new()
                    .with_checkout(checkout)
                    .clone(url.as_ref(), &dir)?
            }
            Ok(_) => {
                log::warn!("Path {:?} exists but is not a directory", dir);
                return Err(Error::NoDirectory(dir));
            }
        };

        log::debug!("Fetching {} in {:?}", branch, dir);
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

    fn repo_dir<R: AsRef<std::path::Path>>(&self, root: R) -> std::path::PathBuf
    where
        std::path::PathBuf: From<R>,
    {
        let mut full_path = std::path::PathBuf::from(root);

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
    dir: std::path::PathBuf,
}

impl CheckedoutJob {
    pub fn run(&self) -> Result<(), Error> {
        let cmd = self
            .job
            .command
            .split(' ')
            .nth(1)
            .map(|cmd| format!("{}.rhai", cmd))
            .ok_or(Error::NoCmd)?;
        let rel_script_path = std::path::Path::new(".github").join(cmd);
        let script_path = self.dir.join(&rel_script_path);

        let engine = rhai::Engine::new();

        log::debug!("Executing {:?} in {:?}", rel_script_path, self.dir);

        match engine.eval_file::<i64>(script_path) {
            Ok(e) => log::debug!("Script result: {}", e),
            Err(e) => log::warn!("Failed to execute script: {:?}", e),
        };

        Ok(())
    }
}
