//! The log routes: Logtail, OTLP and the Vercel drain, which share one path
//! from a raw body to queued log records.

use super::{
    App, Failure, MAX_RECORDS, bad, blocking, body, body::Held, encoding, header_str, limit,
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

/// What became of a log request.
pub(super) enum Outcome {
    /// Its records were queued, or dropped and counted for a signed drain.
    Taken,
    /// Refused unread, with whether its body was drained whole.
    Refused(limit::Limited, body::Drained),
}

impl Outcome {
    /// The answer: `taken`, or a `429` with the body `refused` makes of
    /// why.
    pub(super) fn answer<R: IntoResponse>(
        self,
        taken: impl IntoResponse,
        refused: impl FnOnce(&'static str) -> R,
    ) -> Response {
        match self {
            Self::Taken => taken.into_response(),
            Self::Refused(l, drained) => body::closing(l.refuse(refused(l.message())), &drained),
        }
    }
}

/// Reads, decompresses and parses a log body, which `parse` pays for, and
/// queues its records, counted with the decompressed size of the body.
/// A project past its daily log quota or its log spike rate is refused
/// before the body is read, which is drained and dropped. A signed Vercel
/// drain is read and its signature checked instead, and its records are
/// dropped and counted.
pub(super) async fn ingest_logs(
    app: &App,
    project: i64,
    headers: &HeaderMap,
    raw: Body,
    policy: Arc<Policy>,
    signed: Option<vercel::Signed>,
    parse: impl FnOnce(&[u8], &mut Held) -> Result<Vec<LogRecord>, Failure> + Send + 'static,
) -> Result<Outcome, Failure> {
    let mut held = app.budget.hold();
    let limits = limit::limits(app, project);
    let encoding = encoding(headers);
    if limits.refuses(Signal::Logs) {
        if let Some(signed) = signed {
            let bytes = body::read(raw, &mut held).await?;
            let records = blocking(move || {
                let body = vercel::decode(Some(&signed), bytes, &encoding, &mut held)?;
                Ok(crate::logs::vercel::count(&body))
            })
            .await?;
            let gate = app.usage.gate();
            let reason = limits.reason(Signal::Logs);
            gate.refuse(project, reason, "log_item", records, time::now_ns());
            return Ok(Outcome::Taken);
        }
        if let Some(l) = limit::refused(app, project, Signal::Logs, &limits) {
            let drained = body::drain(raw, headers, body::COMPRESSED).await;
            return Ok(Outcome::Refused(l, drained));
        }
    }
    let bytes = body::read(raw, &mut held).await?;
    let (records, size, _held) = blocking(move || {
        let body = vercel::decode(signed.as_ref(), bytes, &encoding, &mut held)?;
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
    Ok(Outcome::Taken)
}
pub(super) async fn logtail_ingest(State(app): State<App>, req: Request) -> Response {
    let run = async {
        let (parts, raw) = req.into_parts();
        let project = app.keys.authenticate(bearer(&parts.headers))?;
        let content_type = header_str(&parts.headers, header::CONTENT_TYPE).to_owned();
        let outcome = ingest_logs(
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
        .await;
        let outcome = limit::busy(&app, Some(project), outcome)?;
        Ok::<_, Failure>(outcome.answer(StatusCode::ACCEPTED, |why| why))
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
        let outcome = ingest_logs(
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
        .await;
        let outcome = limit::busy(&app, Some(project), outcome)?;
        Ok::<_, Failure>((encoding, outcome))
    };
    let typed = |encoding: otlp::Encoding| [(header::CONTENT_TYPE, encoding.content_type())];
    match (run.await, encoding) {
        (Ok((encoding, outcome)), _) => outcome
            .answer((typed(encoding), otlp::success(encoding)), |why| {
                (typed(encoding), otlp::status(encoding, true, why))
            }),
        (Err((status, message)), Some(encoding)) => {
            let exhausted = status == StatusCode::TOO_MANY_REQUESTS;
            let body = otlp::status(encoding, exhausted, &message);
            (status, typed(encoding), body).into_response()
        }
        (Err(failure), None) => failure.into_response(),
    }
}
