use bankbot::{Job, LocalQueue, Queue};
use std::convert::TryInto;
use std::sync::{Arc, Mutex};
use structopt::StructOpt;
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
}

type State = Arc<Mutex<LocalQueue<String, Job>>>;

async fn remove_from_queue(req: tide::Request<State>) -> tide::Result {
    let queue = req.state();
    match queue.lock() {
        Ok(mut queue) => match queue.remove() {
            Some(job) => Ok(tide::Body::from_json(&job)?.into()),
            None => Ok(tide::Response::builder(404).build()),
        },
        Err(e) => {
            log::warn!("Failed to access queue mutex: {}", e);
            Ok(tide::Response::builder(500).build())
        }
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

    async_std::task::spawn(async move {
        loop {
            if let Ok(mut res) = surf::post(format!("{}/queue/remove", self_url)).await {
                if res.status() == surf::StatusCode::Ok {
                    match res.body_json::<Job>().await {
                        Ok(job) => {
                            job.run();
                        }
                        Err(e) => log::warn!("Failed to retrieve job from queue: {}", e),
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(10));
        }
    });

    app.listen((config.address, config.port)).await?;
    Ok(())
}
