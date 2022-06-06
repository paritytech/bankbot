use std::sync::{Arc, Mutex};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Failed to create comment: {0}")]
    CreateComment(#[from] octocrab::Error),
    #[error("Error calling Github API: {0}")]
    GithubApiError(String),
    #[error("Failed to gain exclusive lock on the octocrab client")]
    ExclusiveLock,
}

pub mod cargo;
pub mod git;

use crate::job::Repository;
#[derive(Clone, Debug)]
pub struct Issue {
    client: Arc<Mutex<octocrab::Octocrab>>,
    repository: Repository,
    issue: octocrab::models::issues::Issue,
}

use std::convert::TryInto;

impl Issue {
    pub fn create_comment<S: AsRef<str>>(
        &mut self,
        body: S,
    ) -> Result<octocrab::models::issues::Comment, Box<rhai::EvalAltResult>> {
        // Unfortunately (like I just found out) octocrab depends on reqwest which depends on
        // tokio. Octocrab has an issue to fix that though, which I just might do :D
        //
        // TODO: Think about ways to re-use the tokio runtime
        // TODO: Fix https://github.com/XAMPPRocky/octocrab/issues/99
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build().map_err(|e| format!("{}", e))?;

        let github_installation_client = match rt.block_on(async {
            // TODO: Get rid of at least the first unwrap (I just introduced it, used to be a ?
            let installations = self.client.lock().unwrap().apps().installations().send().await.unwrap().take_items();
            let mut access_token_req = octocrab::params::apps::CreateInstallationAccessToken::default();
            access_token_req.repository_ids = vec!(self.repository.id);
            // TODO: Properly fill-in installation
            // TODO: Get rid of at least the first unwrap (I just introduced it, used to be a ?
            let access: octocrab::models::InstallationToken = self.client.lock().unwrap().post(installations[0].access_tokens_url.as_ref().unwrap(), Some(&access_token_req)).await?;
            octocrab::OctocrabBuilder::new().personal_token(access.token).build()
        }) {
            Ok(github_installation_client) => github_installation_client,
            _ => { log::warn!("Failed to require octocrab Github client"); return Err(format!("Failed to require octocrab Github client").into())},
        };

        log::debug!("about to get a list of issues");
        rt.block_on( async {
            /*
            let page = self.client
                .lock()
            let page = github_installation_client
                .issues(&self.repository.owner.login, &self.repository.name)
                .list()
                .send()
                .await
                .map_err(|e| e.to_string())?;
                */

            github_installation_client
                .issues(&self.repository.owner.login, &self.repository.name)
                .create_comment(self.issue.number.try_into().map_err(|e: std::num::TryFromIntError| e.to_string())?, body)
                .await
                .map_err(|e| e.to_string().into())
        })
    }

    pub fn new(
        client: Arc<Mutex<octocrab::Octocrab>>,
        repository: Repository,
        issue: octocrab::models::issues::Issue,
    ) -> Self {
        Issue {
            client,
            repository,
            issue,
        }
    }
}

