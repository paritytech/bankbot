use bankbot::{LocalQueue, Queue};
use std::convert::TryInto;
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

#[allow(unused)]
struct Job {
    command: String,
    user: octocrab::models::User,
    repository: octocrab::models::Repository,
    issue: octocrab::models::issues::Issue,
}

#[async_std::main]
async fn main() -> tide::Result<()> {
    let config = Config::from_args();
    pretty_env_logger::formatted_timed_builder()
        .filter(None, config.log_level)
        .init();

    let command_prefix = config.command_prefix.clone();

    let queue = std::sync::RwLock::new(LocalQueue::new());

    let mut app = tide::new();
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

                    match queue.write() {
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

    app.listen((config.address, config.port)).await?;
    Ok(())
}
