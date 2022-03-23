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
}

#[async_std::main]
async fn main() -> tide::Result<()> {
    let config = Config::from_args();
    pretty_env_logger::formatted_timed_builder()
        .filter(None, config.log_level)
        .init();

    let mut app = tide::new();
    let github = tide_github::new(config.webhook_secret)
        .on(Event::IssueComment, |payload| {
            log::debug!("Got payload for repository {}", payload.repository.name);
            if let Some(comment) = payload.comment {
                if let Some(body) = comment.body {
                    log::debug!("payload body: {:?}", body);
                }
            }
        })
        .build();
    app.at("/").nest(github);
    app.listen((config.address, config.port)).await?;
    Ok(())
}
