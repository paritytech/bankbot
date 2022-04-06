use bankbot::{Job, LocalQueue, Queue};
use std::convert::TryInto;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use structopt::StructOpt;
use tide::prelude::*;
use tide_github::Event;

#[derive(Debug, StructOpt)]
#[structopt(name = "bankbot", about = "The benchmarking bot")]
struct Config {
    /// Github Webhook secret
    #[structopt(short, long, env, hide_env_values = true)]
    webhook_secret: String,
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

        let mut queue = match queue.lock() {
            Ok(queue) => queue,
            Err(e) => {
                log::warn!("Failed to access queue mutex: {}", e);
                return Ok(tide::Response::builder(500).build());
            }
        };

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
                        .unwrap_or(body);

                    let id = format!(
                        "{}_{}_{}",
                        payload.repository.name,
                        command,
                        chrono::Utc::now().timestamp_nanos()
                    );

                    let job = Job {
                        command,
                        user: payload.comment.user,
                        repository: payload.repository,
                        issue: payload.issue,
                    };

                    match queue.lock() {
                        Ok(mut queue) => {
                            queue.add(id, job);
                        }
                        Err(e) => {
                            log::warn!("Failed to queue job: {}", e)
                        }
                    }
                }
            }
        })
        .build();
    app.at("/").nest(github);
    app.at("/queue/remove").post(remove_from_queue);

    let self_url = format!("http://{}:{}", config.address, config.port);
    let repos_root = config.repos_root.clone();
    async_std::task::spawn(async move {
        async fn run<P: AsRef<std::path::Path> + AsRef<std::ffi::OsStr>>(
            repos_root: P,
            job: Job,
        ) -> Result<(), String> {
            job.checkout(&repos_root)
                .map_err(|e| format!("{}", e))?
                .run()
                .map_err(|e| format!("{}", e))?;
            Ok(())
        }

        async fn get_job<D: std::fmt::Display>(url: D) -> Result<Job, String> {
            let mut res = surf::post(format!("{}/queue/remove?long_poll=true", url))
                .await
                .map_err(|e| format!("{}", e))?;
            match res.body_json::<Job>().await {
                Ok(job) => Ok(job),
                Err(e) => Err(format!("{}", e)),
            }
        }

        loop {
            match get_job(&self_url).await {
                Ok(job) => {
                    log::info!(
                        "Processing command {} by user {} from repo {}",
                        job.command,
                        job.user.login,
                        job.repository.url
                    );
                    if let Err(e) = run(&repos_root, job).await {
                        log::warn!("Error running job: {}", e);
                    };
                }
                Err(e) => log::warn!("Failed to retrieve job from queue: {}", e),
            }
        }
    });

    app.listen((config.address, config.port)).await?;
    Ok(())
}
