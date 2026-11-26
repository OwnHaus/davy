//! The log routes: Logtail, OTLP and the Vercel drain, which share one path
//! from a raw body to queued log records.

use super::{
    App, Failure, MAX_RECORDS, bad, blocking, body, body::Held, encoding, header_str,
    too_many_records, vercel,
};
use crate::{
    http::sdk::scrubbed_logs,
    logs::{LogRecord, logtail, otlp},
    scrub::Policy,
    time,
    token::bearer,
    usage::{Count, Signal},
};
use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use std::sync::Arc;

/// Reads, decompresses and parses a log body, which `parse` pays for, and
/// queues its records, counted with the decompressed size of the body.
pub(super) async fn ingest_logs(
    app: &App,
    project: i64,
    headers: &HeaderMap,
    raw: Body,
    policy: Arc<Policy>,
    signed: Option<vercel::Signed>,
    parse: impl FnOnce(&[u8], &mut Held) -> Result<Vec<LogRecord>, Failure> + Send + 'static,
) -> Result<(), Failure> {
    let mut held = app.budget.hold();
    let bytes = body::read(raw, &mut held).await?;
    let encoding = encoding(headers);
    let (records, size, _held) = blocking(move || {
        let body = vercel::decode(signed, bytes, &encoding, &mut held)?;
        let mut records = parse(&body, &mut held)?;
        if records.len() > MAX_RECORDS {
            return Err(too_many_records());
        }
        scrubbed_logs(&mut held, |meter| policy.logs(&mut records, meter))?;
        Ok((records, body.len(), held))
    })
    .await?;
    if !records.is_empty() {
        let count = Count::new(records.len(), size);
        app.logs.push(records)?;
        app.usage.add(project, Signal::Logs, count);
    }
    Ok(())
}
pub(super) async fn logtail_ingest(State(app): State<App>, req: Request) -> Response {
    let run = async {
        let (parts, raw) = req.into_parts();
        let project = app.keys.authenticate(bearer(&parts.headers))?;
        let content_type = header_str(&parts.headers, header::CONTENT_TYPE).to_owned();
        ingest_logs(
            &app,
            project,
            &parts.headers,
            raw,
            app.keys.policy(project)?,
            None,
            move |body, held| {
                let refused = |e| (StatusCode::NOT_ACCEPTABLE, e);
                held.take(logtail::cost(body, &content_type).map_err(refused)?)?;
                logtail::parse(body, &content_type, project, time::now_ns()).map_err(refused)
            },
        )
        .await?;
        Ok::<_, Failure>(StatusCode::ACCEPTED.into_response())
    };
    run.await.unwrap_or_else(IntoResponse::into_response)
}
pub(super) async fn otlp_ingest(State(app): State<App>, req: Request) -> Response {
    let (parts, raw) = req.into_parts();
    let encoding =
        otlp::Encoding::from_content_type(header_str(&parts.headers, header::CONTENT_TYPE));
    let run = async {
        let project = app.keys.authenticate(bearer(&parts.headers))?;
        let encoding = encoding.ok_or_else(|| {
            (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "expected application/x-protobuf or application/json".to_owned(),
            )
        })?;
        ingest_logs(
            &app,
            project,
            &parts.headers,
            raw,
            app.keys.policy(project)?,
            None,
            move |body, held| {
                held.take(otlp::cost(body, encoding).map_err(bad)?)?;
                let request = otlp::decode(body, encoding).map_err(bad)?;
                otlp::convert(request, project, time::now_ns(), |bytes| held.take(bytes))
            },
        )
        .await?;
        Ok::<_, Failure>(encoding)
    };
    match (run.await, encoding) {
        (Ok(encoding), _) => (
            [(header::CONTENT_TYPE, encoding.content_type())],
            otlp::success(encoding),
        )
            .into_response(),
        (Err((status, message)), Some(encoding)) => (
            status,
            [(header::CONTENT_TYPE, encoding.content_type())],
            otlp::status(encoding, &message),
        )
            .into_response(),
        (Err(failure), None) => failure.into_response(),
    }
}
