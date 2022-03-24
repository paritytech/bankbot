use structopt::StructOpt;
use tide_github::Event;
use bankbot::{Queue, LocalQueue};
use std::convert::TryInto;
use std::sync::{Arc, Mutex};

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

    let queue = Arc::new(Mutex::new(LocalQueue::new()));

    let mut app = tide::new();
    let github = tide_github::new(&config.webhook_secret)
        .on(Event::IssueComment, move |payload| {
            log::debug!("Received payload for repository {}", payload.repository.name);
            let payload: tide_github::payload::IssueCommentPayload = payload.try_into().unwrap();

            if let Some(body) = payload.comment.body {
                if body.starts_with(&command_prefix) {
                    let command = body.split_once('\n')
                        .map(|(cmd, _)| cmd.into())
                        .unwrap_or(body);

                    let job = Job {
                        command,
                        user: payload.comment.user,
                        repository: payload.repository,
                        issue: payload.issue,
                    };

                    let mut queue = queue.lock().unwrap();
                    queue.enqueue(job).unwrap();
                }
            }
        })
        .build();
    app.at("/").nest(github);

    app.listen((config.address, config.port)).await?;
    Ok(())
}
