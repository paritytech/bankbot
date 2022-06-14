use structopt::StructOpt;
use anyhow::Result;
use thiserror::Error;
use octocrab::Octocrab;
use std::convert::TryInto;

#[derive(Debug, StructOpt)]
#[structopt(name = "bbot-run", about = "Run bbot scripts from the command line, like from a CI/CD job")]
struct Opt {
    /// Path to the repository
    #[structopt(long, env, default_value = "./")]
    repo: std::path::PathBuf,
    /// Path to the directory where the script can clone repositories to
    #[structopt(long, env, default_value = "/tmp")]
    clone_dir: std::path::PathBuf,
    /// Github App ID
    #[structopt(long, env)]
    github_app_id: u64,
    /// Github App key
    #[structopt(long, env, hide_env_values = true)]
    github_app_key: String,
    /// Owner of the upstream Github repository
    #[structopt(long, env)]
    github_owner: String,
    /// Name of the upstream Github repository
    #[structopt(long, env)]
    github_name: String,
    /// Path to the script to execute relative to the root of the script's repository
    #[structopt(env)]
    script: std::path::PathBuf,
    /// Arguments to pass to the script
    #[structopt(env)]
    script_args: Vec<String>,
    /// Log level
    #[structopt(short, long, env, default_value = "info")]
    log_level: log::LevelFilter,
}

#[tokio::main]
async fn main() -> Result<()> {
    let opt = Opt::from_args();
    pretty_env_logger::formatted_timed_builder()
        .filter(None, opt.log_level)
        .init();

    let master_client = get_github_client(opt.github_app_id, &opt.github_app_key)?;
    let gh_client = get_github_repo_client(&master_client, &opt.github_owner, &opt.github_name).await?;
    let gh_repo = get_github_repo(&gh_client, &opt.github_owner, &opt.github_name).await?;
    let command: Vec<String> = {
        let mut x = vec![opt.script.to_string_lossy().into_owned()];
        x.extend(opt.script_args);
        x
    };
    let dir = std::fs::canonicalize(&opt.repo)?;
    let job = bankbot::job::CheckedoutJob {
        command,
        dir,
        clone_dir: opt.clone_dir,
        gh_repo,
        gh_issue: None,
    };
    job.prepare_script(master_client)?.run()?;
    Ok(())
}

fn get_github_client<K: ToString>(github_app_id: u64, github_app_key: K) -> Result<Octocrab> {
    let github_app_key = github_app_key.to_string();
    let token = {
        let app_id = octocrab::models::AppId::from(github_app_id);
        let app_key = jsonwebtoken::EncodingKey::from_rsa_pem(github_app_key.as_bytes())?;
        octocrab::auth::create_jwt(app_id, &app_key)?
    };
    Ok(Octocrab::builder().personal_token(token).build()?)
}

#[derive(Error, Debug)]
enum Error {
    #[error("Failed to acquire access token URL")]
    NoAccessTokenURL,
}

async fn get_github_repo_client<O: AsRef<str>, N: AsRef<str>>(gh_client: &octocrab::Octocrab, _owner: O, _name: N) -> Result<octocrab::Octocrab> {
    // TODO: Consider requesting a token with more fine-grained access.
    // TODO: Figure out what installation to use instead of hardcoding
    use octocrab::params::apps::CreateInstallationAccessToken;
    let installations = gh_client.apps().installations().send().await?.take_items();
    let mut access_token_req = CreateInstallationAccessToken::default();
    access_token_req.repositories = vec!();
    let access_token_url = installations[0].access_tokens_url.as_ref().ok_or(Error::NoAccessTokenURL)?;
    let access: octocrab::models::InstallationToken = gh_client.post(access_token_url, Some(&access_token_req)).await?;
    Ok(octocrab::OctocrabBuilder::new().personal_token(access.token).build()?)
}

async fn get_github_repo<O: AsRef<str>, N: AsRef<str>>(gh_client: &octocrab::Octocrab, owner: O, name: N) -> Result<bankbot::job::Repository> {
    Ok(gh_client.repos(owner.as_ref(), name.as_ref()).get().await?.try_into()?)
}
