use thiserror::Error;
use std::path::{Path, PathBuf};
use git2::build::{CheckoutBuilder, RepoBuilder};
use std::sync::{Arc, Mutex};
use std::convert::TryInto;
use std::convert::TryFrom;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Failed to checkout repository because path {0} exists but is not a directory")]
    NoDirectory(PathBuf),
    #[error("Failed to checkout repository: {source}")]
    Checkout {
        #[from]
        source: git2::Error,
    },
    #[error("Failed to gain exclusive lock to repository")]
    ExclusiveLock,
    #[error("File or directory not found")]
    NotFound,
    #[error("Failed to read or write file")]
    FileIO {
        #[from]
        source: std::io::Error,
    },
    #[error("Unexpected status entry encountered for path {0}")]
    UnexpectedStatusEntry(PathBuf),
    #[error("Failed to retrieve Github access token: {0}")]
    NoAccessToken(String)
}

impl From<std::sync::PoisonError<std::sync::MutexGuard<'_, git2::Repository>>> for Error {
    fn from(_: std::sync::PoisonError<std::sync::MutexGuard<'_, git2::Repository>>) -> Self {
        Self::ExclusiveLock
    }
}

/*
impl Into<Box<rhai::EvalAltResult>> for Error {
    fn into(self) -> Box<rhai::EvalAltResult> {
        format!("{self}").into()
    }
}
*/

#[derive(Clone, Debug)]
pub struct Git {
    /// Path to the repository owning the script
    // TODO: Crate initializer so these don't need `pub`
    pub(crate) path: std::path::PathBuf,
    /// Root containing the repositories
    pub(crate) root: std::path::PathBuf,
    pub(crate) github_client: Arc<Mutex<octocrab::Octocrab>>,
    pub(crate) tokio_handle: tokio::runtime::Handle,
}

impl Git {
    // To make the common case both easy and efficient this function both clones and
    // fetches/checksout a ref.
    pub fn clone<S: AsRef<str>>(&mut self, repo_name: String, head: S) -> Result<LocalRepo, Box<rhai::EvalAltResult>> {
        let url = format!("https://github.com/{}", repo_name);
        let dir = self.repo_dir(&url);
        let repo = match std::fs::metadata(&dir) {
            Ok(metadata) if metadata.is_dir() => git2::Repository::open(&dir).map_err(|e| format!("{e}"))?,
            Err(_) => {
                // Path doesn't exist
                let mut checkout = CheckoutBuilder::new();
                checkout.remove_untracked(true).remove_ignored(true).force();
                log::info!("Cloning {} to {:?}", &url, &dir);
                RepoBuilder::new()
                    .with_checkout(checkout)
                    .clone(url.as_ref(), &dir).map_err(|e| format!("{e}"))?
            }
            Ok(_) => {
                let err = format!("Path {:?} exists but is not a directory", dir);
                log::warn!("{}", err);
                return Err(Box::new(err.into()));
            }
        };
        let repo = LocalRepo::with_repo(dir, repo_name, head.as_ref(), repo, self.github_client.clone(), self.tokio_handle.clone())?;
        log::info!("Constructed local repo {:?}", repo.dir);
        Ok(repo)
    }

    fn repo_dir<U: std::fmt::Display>(&self, url: U) -> PathBuf {
        let mut full_path = PathBuf::from(&self.root);
        let url = format!("{url}").replace('/', "");
        let dir_name = format!(
            "{}__{}",
            self.path.to_string_lossy(),
            &url,
        );
        full_path.set_file_name(dir_name);
        full_path
    }
}

#[derive(Clone)]
pub struct LocalRepo {
    dir: PathBuf,
    repo: Arc<Mutex<git2::Repository>>,
    config: Option<Config>,
    github_client: Arc<Mutex<octocrab::Octocrab>>,
    github_name: String,
    tokio_handle: tokio::runtime::Handle,
}

impl std::fmt::Display for LocalRepo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Local Git repo @ {}", self.dir.display())
    }
}

#[derive(Clone)]
struct Config {
    name: String,
    email: String,
}

impl LocalRepo {
    pub(crate) fn new<P: AsRef<Path>, N: AsRef<str>>(dir: P, repo_name: N, repo: git2::Repository, github: Arc<Mutex<octocrab::Octocrab>>, tokio_handle: tokio::runtime::Handle) -> LocalRepo {
        LocalRepo {
            dir: PathBuf::from(dir.as_ref()),
            repo: Arc::new(Mutex::new(repo)),
            config: None,
            github_name: String::from(repo_name.as_ref()),
            github_client: github,
            tokio_handle,
        }
    }

    fn with_repo<P: AsRef<Path>, S: AsRef<str>, R: AsRef<str>>(dir: P, repo_name: R, head: S, repo: git2::Repository, github_client: Arc<Mutex<octocrab::Octocrab>>, tokio_handle: tokio::runtime::Handle) -> Result<LocalRepo, Box<rhai::EvalAltResult>>
    {
        let mut s = LocalRepo {
            dir: PathBuf::from(dir.as_ref()),
            repo: Arc::new(Mutex::new(repo)),
            config: None,
            github_client,
            github_name: String::from(repo_name.as_ref()),
            tokio_handle,
        };
        s.checkout_remote_head(head.as_ref()).map_err(|e| format!("{e}"))?;
        Ok(s)
    }

    // fetch and checkout/reset remote head (branch)
    fn checkout_remote_head<S: AsRef<str>>(&mut self, head: S) -> Result<(), Error> {
        let head = head.as_ref();
        let repo = self.repo.lock()?;
        log::info!("Fetching {} in {:?}", head, self.dir);
        //self.repo.lock()?.find_remote("origin")?.fetch(
        let mut remote = repo.find_remote("origin")?;
        remote.fetch(
            &[&format!("refs/{}:refs/heads/{}", head, head)],
            None,
            None,
        )?;

        //let repo = self.repo.lock()?;
        let rev = repo.revparse_single(head)?;
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

        Ok(())
    }

    // Checkout a possibly new local branch
    pub fn checkout_new_branch<S: AsRef<str>>(&mut self, name: S) -> Result<(), Error> {
        self.checkout_new_branch_target(name, "HEAD")
    }

    pub fn checkout_new_branch_target<N: AsRef<str>, T: AsRef<str>>(&mut self, name: N, target: T) -> Result<(), Error> {
        let repo = self.repo.lock()?;
        let target_obj = repo.revparse_ext(target.as_ref())?;
        let target = target_obj.0.peel_to_commit()?;
        repo.branch(name.as_ref(), &target, false)?;
        Ok(())
    }

    // TODO: Accept a NormalizedPath parameter and implement From<AsRef<Path>> for it.
    fn normalize_path<P: AsRef<Path>>(&self, path: P) -> Result<PathBuf, Error> {
        let path = path.as_ref();
        let path = if path.is_relative() {
            self.dir.join(path)
        } else {
            path.to_path_buf()
        };
        match path.canonicalize() {
            Ok(path) if path.starts_with(&self.dir) => {
                Ok(path)
            },
            _ => Err(Error::NotFound)
        }
    }

    // TODO: Get rid of all the `.map_err(|e| format!("{e}"))` with an
    // `impl Into<Box<rhai::EvalAltResult>>` or something.

    // NOTE: every function available in rhai should receive `&mut self`
    pub fn read_file<P: AsRef<Path>>(&mut self, path: P) -> Result<Vec<u8>, Box<rhai::EvalAltResult>> {
        let path = path.as_ref();
        log::debug!("Reading file (before normalization): {:?}", path);
        let path = self.get_full_path(path)?;
        log::debug!("Reading file {:?}", path);
        let bytes = std::fs::read(&path).map_err(|e| format!("{e}"))?;
        log::debug!("Read file {:?}", path);
        Ok(bytes)
        //Ok(std::fs::read(path).map_err(|e| format!("{e}"))?)
    }

    //pub fn write_file<P: AsRef<Path>, B: AsRef<[u8]>>(&mut self, path: P, contents: B) -> Result<(), Box<rhai::EvalAltResult>> {
    pub fn write_file<P: AsRef<Path>>(&mut self, path: P, contents: rhai::Blob) -> Result<(), Box<rhai::EvalAltResult>> {
        let path = path.as_ref();
        log::debug!("Writing file (before normalization): {:?}", path);
        let path = self.get_full_path(path)?;
        log::debug!("Writing file {:?}", path);
        // TODO: Make sure directory exists
        Ok(std::fs::write(path, contents).map_err(|e| format!("{e}"))?)
    }

    fn get_full_path<P: AsRef<Path>>(&self, path: P) -> Result<PathBuf, Box<rhai::EvalAltResult>> {
        match self.normalize_path(self.dir.join(&path)).map_err(|e| format!("{e}")) {
            Ok(path) if path.starts_with(&self.dir) => Ok(path),
            Ok(path) => Err(format!("Path leads outside root: {}", path.to_string_lossy()).into()),
            Err(err) => Err(err.into()),
        }
    }

    pub fn list_files(&mut self) -> Result<rhai::Dynamic, Box<rhai::EvalAltResult>> {
        self.list_files_in_dir("./")
    }

    // `list_files` and `list_files_in_dir` will register to the same function (with an
    // optional parameter)
    // NOTE: every function available in rhai should receive `&mut self`
    pub fn list_files_in_dir<P: AsRef<Path>>(&mut self, dir: P) -> Result<rhai::Dynamic, Box<rhai::EvalAltResult>> {
        let path = self.get_full_path(dir)?;
        log::debug!("More specifically, listing files in {:?}", path);
        Ok(
            std::fs::read_dir(path).map_err(|e| format!("{e}"))?
                .filter_map(|entry| {
                    match entry {
                        Ok(entry) => {
                            let metadata = entry.metadata().ok()?;
                            let path = entry.path().strip_prefix(&self.dir).ok()?.to_path_buf();
                            Some(DirEntry {
                                metadata,
                                path: DirEntryPath(path),
                            })
                            //DirEntry::try_from(entry).ok()
                        }
                        Err(_) => None,
                    }
                })
                .collect::<Vec<_>>().into()
        )
    }

    pub fn add<P: AsRef<Path>>(&mut self, path: P) -> Result<(), Box<rhai::EvalAltResult>> {
        let path = path.as_ref();
        //log::debug!("adding path (in dir {:?}) (before normalization): {:?}", self.dir, path);
        //let path = self.normalize_path(path).map_err(|e| format!("{e}"))?;
        //let path = self.get_full_path(path)?;
        log::debug!("adding path (after normalization): {:?}", path);
        let repo = self.repo.lock().map_err(|e| format!("{e}"))?;
        let mut index = repo.index().map_err(|e| format!("{e}"))?;
        index.add_path(path).map_err(|e| format!("{e}").into())
    }

    pub fn add_list<'a, I: IntoIterator<Item = &'a Path>>(&mut self, paths: I) -> Result<(), Box<rhai::EvalAltResult>> {
        let mut errors = vec!();
        paths.into_iter().for_each(|path| {
            if let Err(err) = self.add(path).map_err(|e| format!("{e}")) {
                errors.push(err);
            };
        });
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors[0].clone().into())
        }
    }

    fn commit<S: AsRef<str>>(&mut self, message: S) -> Result<(), Error> {
        let repo = self.repo.lock()?;
        let signature = match &self.config {
            Some(Config{name, email}) => git2::Signature::now(name, email)?,
            None => git2::Signature::now("bankbot (TODO: Changeme)", "changeme@parity.io")?,
        };
        let rev = repo.revparse_single("HEAD")?;
        let commit = rev.peel_to_commit()?;
        let tree = commit.tree()?;
        repo.commit(Some("HEAD"), &signature, &signature, message.as_ref(), &tree, &[&commit])?;
        Ok(())
    }

    pub fn pub_commit<S: AsRef<str>>(&mut self, message: S) -> Result<(), Box<rhai::EvalAltResult>> {
        self.commit(message).map_err(|e| format!("{e}").into())
    }

    pub fn list_modified(&self) -> Result<Vec<PathBuf>, Box<rhai::EvalAltResult>> {
        let repo = self.repo.lock().map_err(|e| format!("{e}"))?;
        let list = repo.statuses(Some(git2::StatusOptions::default().include_unmodified(false))).map_err(|e| format!("{e}"))?
            .iter()
            .filter_map(|entry| entry.path().map(PathBuf::from))
            .collect();
        Ok(list)
    }

    fn push<R: AsRef<str>>(&mut self, gitref: R) -> Result<(), Error> {
        log::debug!("pushing!");
        let repo = self.repo.lock()?;
        let mut remote = repo.find_remote("origin")?;
        let github_client = self.github_client.lock().map_err(|_| Error::ExclusiveLock)?;
        // TODO: Fix block_on
        let access_token_res: Result<String, Error> = self.tokio_handle.block_on(async {
            log::debug!("Getting installations...");
            let installations = github_client.apps().installations().send().await.unwrap().take_items();
            log::debug!("Got some installations!");
            let mut access_token_req = octocrab::params::apps::CreateInstallationAccessToken::default();
            access_token_req.repositories = vec!();
            // TODO: Properly fill-in installation
            let access: octocrab::models::InstallationToken = github_client.post(installations[0].access_tokens_url.as_ref().unwrap(), Some(&access_token_req)).await.map_err(|e| Error::NoAccessToken(format!("{e}")))?;
            Ok(access.token)
        });
        let access_token = access_token_res?;
        log::debug!("Got an access token!");
        let mut callbacks = git2::RemoteCallbacks::new();
        callbacks.credentials(move |_url, _username_from_url, _allowed_types| {
            git2::Cred::userpass_plaintext("x-access-token", &access_token)
        });
        let mut push_options = git2::PushOptions::new();
        push_options.remote_callbacks(callbacks);
        log::debug!("push options including creds callback ready!");
        // TODO: Check if this error handling is sufficient
        //Ok(remote.push::<String>(&[String::from(gitref.as_ref())], Some(&mut push_options))?)
        if let Err(err) = remote.push::<String>(&[format!("refs/heads/{}", gitref.as_ref())], Some(&mut push_options)) {
            log::debug!("Failed to push: {err}");
            Err(err)?
        } else {
            Ok(())
        }
    }

    pub fn pub_push<R: AsRef<str>>(&mut self, gitref: R) -> Result<(), Box<rhai::EvalAltResult>> {
        self.push(gitref).map_err(|e| format!("{e}").into())
    }

    fn status(&self) -> Result<Status, Error> {
        let repo = self.repo.clone();
        let statuses = {
            let repo = self.repo.lock()?;
            let x = repo.statuses(None)?.iter().filter_map(|entry| entry.try_into().ok()).collect::<Vec<StatusEntry>>();
            x
        };
        Ok(Status{repo, statuses})
    }

    pub fn pub_status(&mut self) -> Result<Status, Box<rhai::EvalAltResult>> {
        self.status().map_err(|e| format!("{e}").into())
    }
}

#[derive(Clone)]
struct StatusEntry {
    path: PathBuf,
    status: git2::Status,
}

impl TryFrom<git2::StatusEntry<'_>> for StatusEntry {
    type Error = String;
    fn try_from(entry: git2::StatusEntry) -> Result<StatusEntry, String> {
        let entry = StatusEntry {
            path: entry.path().ok_or_else(|| "Non-utf8 file path not supported".to_string())?.into(),
            status: entry.status(),
        };
        Ok(entry)
    }
}

#[derive(Clone)]
pub struct Status {
    repo: Arc<Mutex<git2::Repository>>,
    statuses: Vec<StatusEntry>,
}

#[derive(Clone, Debug)]
pub struct DirEntryPath(PathBuf);

impl DirEntryPath {
    pub fn strip_prefix<P: AsRef<Path>>(&mut self, prefix: P) -> DirEntryPath {
        match self.0.strip_prefix(prefix) {
            Ok(p) => DirEntryPath(p.to_path_buf()),
            Err(_) => DirEntryPath(self.0.clone()),
        }
    }
}

impl AsRef<Path> for DirEntryPath {
    fn as_ref(&self) -> &Path {
        self.0.as_path()
    }
}

#[derive(Clone, Debug)]
pub struct DirEntry {
    pub path: DirEntryPath,
    metadata: std::fs::Metadata,
}

impl DirEntry {
    pub fn is_file(&mut self) -> bool {
        self.metadata.is_file()
    }

    pub fn is_dir(&mut self) -> bool {
        self.metadata.is_dir()
    }

    pub fn is_symlink(&mut self) -> bool {
        self.metadata.is_symlink()
    }

    pub fn get_path(&mut self) -> DirEntryPath {
        self.path.clone()
    }
}

#[derive(Clone)]
pub struct File {
    pub path: PathBuf,
    pub repo: Arc<Mutex<git2::Repository>>,
}


impl Status {
    pub fn pub_changed(&mut self) -> Result<rhai::Dynamic, Box<rhai::EvalAltResult>> {
        self.changed().map(|e| e.into()).map_err(|e| format!("{e}").into())
    }

    fn changed(&self) -> Result<Vec<DirEntryPath>, Error> {
        let files = self.statuses.iter().filter(|entry| {
            entry.status.is_wt_modified() || entry.status.is_wt_renamed() || entry.status.is_wt_typechange()
        //}).map(|entry| File { path: entry.path.clone(), repo: self.repo.clone()}).collect();
        }).map(|entry| DirEntryPath(entry.path.clone())).collect();
        Ok(files)
    }

    pub fn pub_added(&mut self) -> Result<rhai::Dynamic, Box<rhai::EvalAltResult>> {
        self.added().map(|e| e.into()).map_err(|e| format!("{e}").into())
    }

    fn added(&self) -> Result<Vec<DirEntryPath>, Error> {
        let files = self.statuses.iter().filter(|entry| {
            entry.status.is_wt_new()
        //}).map(|entry| File { path: entry.path.clone(), repo: self.repo.clone() }).collect();
        }).map(|entry| DirEntryPath(entry.path.clone())).collect();
        Ok(files)
    }

    pub fn pub_deleted(&mut self) -> Result<rhai::Dynamic, Box<rhai::EvalAltResult>> {
        self.deleted().map(|e| e.into()).map_err(|e| format!("{e}").into())
    }

    fn deleted(&self) -> Result<Vec<DirEntryPath>, Error> {
        let files = self.statuses.iter().filter(|entry| {
            entry.status.is_wt_deleted()
        //}).map(|entry| File{ path: entry.path.clone(), repo: self.repo.clone() }).collect();
        }).map(|entry| DirEntryPath(entry.path.clone())).collect();
        Ok(files)
    }
}
