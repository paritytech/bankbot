use async_std::sync::{Arc, Mutex};
use ci_script::{job::Repository, Job, LocalQueue, Queue};
use octocrab::params::apps::CreateInstallationAccessToken;
use octocrab::Octocrab;
use std::convert::TryInto;
use std::path::{Path, PathBuf};
use structopt::StructOpt;
use thiserror::Error;
use tide::prelude::*;
use tide_github::Event;

#[derive(Debug, StructOpt)]
#[structopt(
    name = "ci-script",
    about = "Simply automate your CI needs with the powers of the CI Scripting Language"
)]
struct Config {
    /// Github Webhook secret
    #[structopt(short, long, env, hide_env_values = true)]
    webhook_secret: String,
    /// Github App ID
    #[structopt(long, env)]
    app_id: u64,
    /// Github App key
    #[structopt(long, env, hide_env_values = true)]
    app_key: String,
    /// Port to listen on
    #[structopt(short, long, env, default_value = "3000")]
    port: u16,
    /// Address to listen on
    #[structopt(short, long, env, default_value = "127.0.0.1")]
    address: String,
    /// Log level
    #[structopt(short, long, env, default_value = "info")]
    log_level: log::LevelFilter,
    /// Bot command prefix
    #[structopt(short, long, env, default_value = "/benchbot")]
    command_prefix: String,
    /// Repositories root working directory
    #[structopt(short, long, env, default_value = "./repos")]
    repos_root: PathBuf,
}

type State = Arc<Mutex<LocalQueue<String, Job>>>;

#[derive(Error, Debug)]
enum Error {
    #[error("Missing bot command")]
    NoCmd,
}

async fn remove_from_queue(req: tide::Request<State>) -> tide::Result {
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Options {
        long_poll: bool,
    }

    // We lock the Mutex in a separate scope so it can be unlocked (dropped)
    // before we try to .await another future (MutexGuard is not Send).
    let recv = {
        let queue = req.state();

        let mut queue = queue.lock().await;

        match queue.remove() {
            Some(job) => return Ok(tide::Body::from_json(&job)?.into()),
            None => {
                let Options { long_poll } = req.query()?;
                if long_poll {
                    let (send, recv) = async_std::channel::bounded(1);
                    queue.register_watcher(send);
                    Some(recv)
                } else {
                    None
                }
            }
        }
    };

    match recv {
        Some(recv) => {
            let mut res = tide::Response::new(200);
            let job = recv.recv().await?;
            res.set_body(tide::Body::from_json(&job)?);
            Ok(res)
        }
        None => Ok(tide::Response::builder(404).build()),
    }
}

fn prepare_command(command: Vec<String>) -> Result<Vec<String>, Error> {
    // The first argument (.e.g `/bot` is also the name of the directory the script is in
    let dir = command
        .iter()
        .next()
        .map(|cmd| {
            if let Some(cmd) = cmd.strip_prefix('/') {
                String::from(cmd)
            } else {
                String::from(cmd)
            }
        })
        .ok_or(Error::NoCmd)?;
    let file = command
        .iter()
        .nth(1)
        .map(|cmd| format!("{}.rhai", cmd))
        .ok_or(Error::NoCmd)?;
    let mut args: Vec<String> = command.into_iter().skip(2).collect();
    let script_path = String::from(Path::new(".github").join(dir).join(file).to_string_lossy());
    let mut res = vec![script_path];
    res.append(&mut args);
    Ok(res)
}

#[async_std::main]
async fn main() -> tide::Result<()> {
    let config = Config::from_args();
    pretty_env_logger::formatted_timed_builder()
        .filter(None, config.log_level)
        .init();

    let command_prefix = config.command_prefix.clone();

    let queue = Arc::new(Mutex::new(LocalQueue::new()));

    let mut app = tide::with_state(queue.clone());
    let github = tide_github::new(&config.webhook_secret)
        .on(Event::IssueComment, move |payload| {
            let payload: tide_github::payload::IssueCommentPayload = match payload.try_into() {
                Ok(payload) => payload,
                Err(e) => {
                    log::warn!("Failed to parse payload: {}", e);
                    return;
                }
            };

            if let Some(body) = payload.comment.body {
                if body.starts_with(&command_prefix) {
                    let command = body
                        .split_once('\n')
                        .map(|(cmd, _)| cmd.into())
                        .map(|cmd| {
                            shell_words::split(cmd).expect("Failed to split command as shell words")
                        })
                        .unwrap_or_else(|| body.split(" ").map(|x| x.to_string()).collect());

                    let command = match prepare_command(command) {
                        Ok(command) => command,
                        Err(e) => {
                            log::warn!("Failed to determine command: {e}");
                            return;
                        }
                    };

                    let id = format!(
                        "{}_{}_{}",
                        payload.repository.name,
                        command.join(" "),
                        uuid::Uuid::new_v4(),
                    );

                    let repo: Repository = match payload.repository.try_into() {
                        Ok(repo) => repo,
                        Err(err) => {
                            log::warn!("Failed to parse repository payload: {}", err);
                            return;
                        }
                    };

                    let job = Job {
                        command,
                        // user: payload.comment.user,
                        repository: repo,
                        issue: payload.issue,
                    };

                    let q = queue.clone();
                    async_std::task::spawn(async move {
                        q.lock().await.add(id, job);
                    });
                }
            }
        })
        .build();
    app.at("/").nest(github);
    app.at("/queue/remove").post(remove_from_queue);

    let self_url = format!("http://{}:{}", config.address, config.port);
    let repos_root = config.repos_root.clone();
    let github_client = {
        let token = {
            let app_id = octocrab::models::AppId::from(config.app_id);
            let app_key = jsonwebtoken::EncodingKey::from_rsa_pem(config.app_key.as_bytes())?;
            octocrab::auth::create_jwt(app_id, &app_key)?
        };
        Octocrab::builder().personal_token(token).build()?
    };

    let tokio_rt = tokio::runtime::Runtime::new()?;
    async_std::task::spawn(async move {
        async fn run<P: AsRef<std::path::Path> + AsRef<std::ffi::OsStr>>(
            repos_root: P,
            job: Job,
            github_client: octocrab::Octocrab,
            //tokio_handle: tokio::runtime::Handle,
        ) -> anyhow::Result<()> {
            //let github = Arc::try_unwrap(github_client).into_inner();
            //let github = std::sync::Arc::new(std::sync::Mutex::new(github));
            job.checkout(&repos_root)?
                .prepare_script(github_client)?
                .run()?;
            Ok(())
        }

        async fn get_job<D: std::fmt::Display>(url: D) -> anyhow::Result<Job> {
            let mut res = surf::post(format!("{}/queue/remove?long_poll=true", url))
                .await
                .map_err(|e| e.into_inner())?;
            res.body_json::<Job>().await.map_err(|e| e.into_inner())
        }

        let rt_handle = tokio_rt.handle();
        loop {
            let github_client = github_client.clone();
            match get_job(&self_url).await {
                Ok(ref job) => {
                    log::info!(
                        "Processing command {} in repo {}",
                        job.command.join(" "),
                        job.repository.url
                    );

                    // TODO: Fix block_on
                    let gh_client = github_client.clone();
                    let github_installation_client = match rt_handle.block_on(async move {
                        let installations = gh_client
                            .apps()
                            .installations()
                            .send()
                            .await
                            .unwrap()
                            .take_items();
                        let mut access_token_req = CreateInstallationAccessToken::default();
                        access_token_req.repository_ids = vec![job.repository.id];
                        // TODO: Properly fill-in installation
                        let access: octocrab::models::InstallationToken = gh_client
                            .post(
                                installations[0].access_tokens_url.as_ref().unwrap(),
                                Some(&access_token_req),
                            )
                            .await?;
                        octocrab::OctocrabBuilder::new()
                            .personal_token(access.token)
                            .build()
                    }) {
                        Ok(github_installation_client) => github_installation_client,
                        _ => {
                            log::warn!("Failed to require octocrab Github client");
                            return;
                        }
                    };

                    let repo_owner = job.repository.owner.login.clone();
                    let repo_name = job.repository.name.clone();
                    let issue_nr = job.issue.number.try_into();

                    let gh_client = github_client.clone();
                    let job = job.clone();
                    //if let Err(job_err) = run(&repos_root, job, gh_client, rt_handle.clone()).await {
                    if let Err(job_err) = run(&repos_root, job, gh_client).await {
                        log::warn!("Error running job: {job_err}");

                        // TODO: create separate tokio threadpool and send messages to
                        // it
                        if let Ok(issue_nr) = issue_nr {
                            match rt_handle.block_on(async {
                                github_installation_client
                                    .issues(&repo_owner, &repo_name)
                                    .create_comment(
                                        issue_nr,
                                        format!("Error running job: {job_err}"),
                                    )
                                    .await
                            }) {
                                Ok(_) => {}
                                Err(err) => log::warn!("Failed to comment on issue: {err}"),
                            };
                        };
                    };
                }
                Err(e) => log::warn!("Failed to retrieve job from queue: {}", e),
            }
        }
    });

    app.listen((config.address, config.port)).await?;
    Ok(())
}
