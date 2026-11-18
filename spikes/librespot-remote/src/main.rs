use librespot_core::{authentication::Credentials, config::SessionConfig, session::Session};
use librespot_oauth::OAuthClientBuilder;
use std::{env, process::ExitCode};

const HELP: &str = "librespot-remote-spike\n\nUsage: librespot-remote-spike [--command pause|play|next|prev --device ID]\n\nSet SPOTIFY_REFRESH_TOKEN, SPOTIFY_CLIENT_ID and SPOTIFY_REDIRECT_URI.\nDevice listing, playback state and remote commands are not implemented: their\nundocumented Connect protocol requirements have not been established.\nNo playback device is registered or playback transferred.";

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<_> = env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{HELP}");
        return ExitCode::SUCCESS;
    }
    if !args.is_empty() {
        eprintln!("Remote commands are unsupported; no request was sent.\n{HELP}");
        return ExitCode::FAILURE;
    }
    match connect().await {
        Ok(()) => {
            eprintln!("Session connected, but device listing and playback state are unsupported; no device was registered.");
            ExitCode::FAILURE
        }
        Err(err) => {
            eprintln!("Session setup failed: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn connect() -> Result<(), Box<dyn std::error::Error>> {
    let refresh = env::var("SPOTIFY_REFRESH_TOKEN")?;
    let client_id = env::var("SPOTIFY_CLIENT_ID")?;
    let redirect = env::var("SPOTIFY_REDIRECT_URI")?;
    let oauth = OAuthClientBuilder::new(&client_id, &redirect, vec!["streaming"]).build()?;
    let token = oauth.refresh_token_async(&refresh).await?;
    let mut config = SessionConfig::default();
    config.client_id = client_id;
    let session = Session::new(config, None);
    session.connect(Credentials::with_access_token(token.access_token), false).await?;
    Ok(())
}
