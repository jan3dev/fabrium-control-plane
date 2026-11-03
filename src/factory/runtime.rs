use std::sync::Arc;

use tokio::sync::Mutex;

use crate::{
    config::FactoryConfig,
    github::{GithubApi, GithubApp},
    matrix::MatrixClient,
};

/// Shared clients and config for webhook dispatch and the reconciler.
#[derive(Clone)]
pub struct FactoryRuntime {
    pub config: Arc<FactoryConfig>,
    pub github_app: Arc<GithubApp>,
    pub github_api: Arc<GithubApi>,
    pub matrix: Arc<MatrixClient>,
    /// Held for one whole dispatch. Two suites on one commit, or `opened` and
    /// `labeled` on one issue, arrive together and must not both start a step.
    pub dispatch_lock: Arc<Mutex<()>>,
}

impl FactoryRuntime {
    pub fn new(config: FactoryConfig) -> anyhow::Result<Self> {
        Self::with_github_api(config, GithubApi::new())
    }

    /// For tests against a stub GitHub API.
    pub fn with_github_api(config: FactoryConfig, github_api: GithubApi) -> anyhow::Result<Self> {
        let github_app = GithubApp::new(
            config.github.app_id,
            &config.github.private_key_pem,
            github_api.clone(),
        )?;
        let matrix = MatrixClient::new(
            config.matrix.homeserver.clone(),
            config.matrix.access_token.clone(),
            config.matrix.implementer.clone(),
            config.matrix.reviewer.clone(),
        );
        Ok(Self {
            config: Arc::new(config),
            github_app: Arc::new(github_app),
            github_api: Arc::new(github_api),
            matrix: Arc::new(matrix),
            dispatch_lock: Arc::default(),
        })
    }
}
