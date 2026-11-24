use super::{App, Failure, HeartbeatToken, Job, Path, State, StatusCode, store, time};

pub(super) async fn ping(
    State(app): State<App>,
    Path(token): Path<String>,
) -> Result<&'static str, Failure> {
    heartbeat(&app, &token, false).await
}
pub(super) async fn fail(
    State(app): State<App>,
    Path(token): Path<String>,
) -> Result<&'static str, Failure> {
    heartbeat(&app, &token, true).await
}
/// The token is the credential, so a malformed and an unknown one get the
/// same `404`.
async fn heartbeat(app: &App, token: &str, failed: bool) -> Result<&'static str, Failure> {
    let unknown = || {
        (
            StatusCode::NOT_FOUND,
            "no heartbeat with this token".to_owned(),
        )
    };
    let token = HeartbeatToken::parse(token).ok_or_else(unknown)?;
    let monitor = app.keys.monitor_for(&token)?.ok_or_else(unknown)?;
    let at = time::now_ms();
    store::submit(
        &app.sender,
        Job::Ping {
            monitor,
            at,
            failed,
        },
    )
    .await?;
    Ok("ok")
}
