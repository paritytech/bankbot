use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Job {
    pub command: String,
    pub user: octocrab::models::User,
    pub repository: octocrab::models::Repository,
    pub issue: octocrab::models::issues::Issue,
}
