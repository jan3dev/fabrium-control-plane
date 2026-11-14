use std::sync::Arc;

use anyhow::Result;
use fabrium_control_plane::{
    AppState, build_app,
    config::Config,
    db,
    ec2::AwsEc2,
    factory::{FactoryRuntime, reconciler},
};
use tokio::{net::TcpListener, signal};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = Config::from_env()?;
    let db = db::connect(&config.database_url).await?;
    let vm = &config.vm;
    tracing::info!(
        region = %vm.aws_region,
        ami = %vm.ami_name,
        security_group = %vm.security_group,
        key_name = %vm.key_name,
        instance_types = ?vm.instance_types,
        default_instance_type = %vm.default_instance_type,
        public_url = %vm.public_url,
        "vm settings"
    );
    let ec2 = Arc::new(AwsEc2::connect(vm).await);
    let bind_addr = config.bind_addr;
    let mut state = AppState::new(db, config.api_keys, config.vm, ec2);
    if let Some(factory) = config.factory {
        tracing::info!(
            repos = ?factory.github.factory_repos,
            app_id = factory.github.app_id,
            "factory enabled"
        );
        let runtime = Arc::new(FactoryRuntime::new(factory)?);
        reconciler::spawn(runtime.clone(), state.db.clone());
        state.factory = Some(runtime);
    }
    let app = build_app(state);

    let listener = TcpListener::bind(bind_addr).await?;
    tracing::info!(addr = %bind_addr, "listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await?;
    tracing::info!("shut down");
    Ok(())
}

async fn shutdown() {
    let ctrl_c = async { signal::ctrl_c().await.expect("install Ctrl-C handler") };
    #[cfg(unix)]
    let term = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
}
