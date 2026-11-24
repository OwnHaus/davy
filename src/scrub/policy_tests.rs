use super::*;
use crate::store::{self, Job, Write, Writer};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Instant,
};
use tower::ServiceExt;

const DEFAULTS: &str = r#"{"keys":true,"credentials":true,"cards":true,"email":false,
    "personal":false,"ip":true,"extra_keys":[]}"#;

/// A data directory with project `a` (id 1), its writer, and the scrub API
/// with the admin token `admin`.
struct Api {
    data: PathBuf,
    writer: Writer,
    router: Router,
    policies: Arc<settings::Cache>,
}

fn api() -> Api {
    let data = crate::test_support::scratch("scrub-policy");
    let c = store::open(&data).unwrap();
    store::create_project(&c, "a").unwrap();
    let writer = Writer::start(c, 8);
    let policies = Arc::new(settings::Cache::default());
    let router = settings::router(
        store::open(&data).unwrap(),
        writer.sender.clone(),
        crate::token::Token::new("admin"),
        Arc::clone(&policies),
    );
    Api {
        data,
        writer,
        router,
        policies,
    }
}

impl Api {
    async fn send(&self, method: &str, uri: &str, auth: Option<&str>, body: &str) -> (u16, String) {
        let mut request = Request::builder().method(method).uri(uri);
        if let Some(auth) = auth {
            request = request.header("authorization", auth);
        }
        let response = self
            .router
            .clone()
            .oneshot(request.body(Body::from(body.to_owned())).unwrap())
            .await
            .unwrap();
        let status = response.status().as_u16();
        let body = to_bytes(response.into_body(), 16_000).await.unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    async fn put(&self, body: &str) -> (u16, String) {
        self.send("PUT", "/api/projects/1/scrub", Some("Bearer admin"), body)
            .await
    }

    fn stored(&self) -> Policy {
        settings::read(&store::open(&self.data).unwrap(), 1).unwrap()
    }

    fn finish(self) {
        drop(self.router);
        drop(self.writer.sender);
        drop(self.writer.ui);
        self.writer.thread.join().unwrap();
        std::fs::remove_dir_all(self.data).unwrap();
    }
}

fn policy(json: &Value) -> String {
    let mut full: Value = serde_json::from_str(DEFAULTS).unwrap();
    for (key, value) in json.as_object().unwrap() {
        full[key] = value.clone();
    }
    full.to_string()
}

#[tokio::test]
async fn the_api_needs_the_admin_token() {
    let api = api();
    for auth in [None, Some("Bearer wrong")] {
        for method in ["GET", "PUT"] {
            let (status, _) = api
                .send(method, "/api/projects/1/scrub", auth, DEFAULTS)
                .await;
            assert_eq!(status, 401, "{method} {auth:?}");
        }
    }
    api.finish();
}

#[tokio::test]
async fn the_api_tightens_a_policy_through_the_writer() {
    let api = api();
    let tighter = policy(&json!({"email": true, "extra_keys": ["tenant-pin"]}));
    let (status, body) = api.put(&tighter).await;
    assert_eq!(status, 200, "{body}");
    let expected = Policy {
        email: true,
        extra_keys: HashSet::from(["tenantpin".to_owned()]),
        ..Policy::default()
    };
    assert_eq!(serde_json::from_str::<Policy>(&body).unwrap(), expected);
    assert_eq!(api.stored(), expected);
    let (status, body) = api
        .send("GET", "/api/projects/1/scrub", Some("Bearer admin"), "")
        .await;
    assert_eq!(
        (status, serde_json::from_str::<Policy>(&body).unwrap()),
        (200, expected)
    );
    api.finish();
}

#[tokio::test]
async fn the_api_refuses_to_weaken_a_policy() {
    let api = api();
    let tighter = policy(&json!({"email": true, "extra_keys": ["tenant-pin", "ssn2"]}));
    assert_eq!(api.put(&tighter).await.0, 200);
    let weaker = policy(&json!({"ip": false, "extra_keys": ["tenant-pin"]}));
    let (status, body) = api.put(&weaker).await;
    assert_eq!(status, 403);
    assert_eq!(
        body,
        "the API only tightens scrubbing, and this policy turns off email, turns off ip, \
         removes extra key ssn2. To weaken it, run `davy project scrub <slug> --data <dir> \
         --settings '<json>'` where the data directory is"
    );
    assert!(api.stored().email, "nothing was stored");
    api.finish();
}

#[tokio::test]
async fn the_api_refuses_incomplete_or_unknown_policies() {
    let api = api();
    let cases = [
        (
            r#"{"email":true}"#.to_owned(),
            "a policy must name every field, and this one leaves out keys, credentials, \
             cards, personal, ip, extra_keys",
        ),
        (
            policy(&json!({"ips": false})),
            "unknown field `ips`, expected one of `keys`, `credentials`, `cards`, `email`, \
             `personal`, `ip`, `extra_keys`",
        ),
        (
            policy(&json!({"extra_keys": ["--"]})),
            "extra key \"--\" has no ASCII letter or digit, and keys are compared by those alone",
        ),
        (
            policy(&json!({"extra_keys": ["p%61ss"]})),
            "extra key \"p%61ss\" holds % or +",
        ),
        (
            policy(&json!({"extra_keys": ["x".repeat(101)]})),
            "must hold 1 to 100 bytes",
        ),
        (
            policy(&json!({"extra_keys": vec!["k"; 101]})),
            "at most 100 extra keys, not 101",
        ),
    ];
    for (body, message) in cases {
        let (status, reply) = api.put(&body).await;
        assert_eq!(status, 400, "{body}");
        assert!(reply.contains(message), "{reply}");
    }
    assert_eq!(api.stored(), Policy::default());
    api.finish();
}

#[tokio::test]
async fn a_policy_for_a_missing_project_is_not_found() {
    let api = api();
    let (status, body) = api
        .send(
            "PUT",
            "/api/projects/99/scrub",
            Some("Bearer admin"),
            DEFAULTS,
        )
        .await;
    assert_eq!((status, body.as_str()), (404, "no project 99"));
    let (status, _) = api
        .send("GET", "/api/projects/99/scrub", Some("Bearer admin"), "")
        .await;
    assert_eq!(status, 404);
    api.finish();
}

#[test]
fn a_policy_for_a_missing_project_fails_alone_in_its_batch() {
    let data = crate::test_support::scratch("scrub-batch");
    let mut c = store::open(&data).unwrap();
    store::create_project(&c, "a").unwrap();
    let tighter = Policy {
        email: true,
        ..Policy::default()
    };
    let (batch, _replies): (Vec<Write>, Vec<_>) = [99, 1]
        .into_iter()
        .map(|project| {
            let (done, reply) = tokio::sync::oneshot::channel();
            let job = Job::Scrub {
                project,
                policy: tighter.clone(),
            };
            (Write { job, done }, reply)
        })
        .unzip();
    assert_eq!(
        store::write_batch(&mut c, &batch),
        [Err("no project 99".to_owned()), Ok(())]
    );
    assert_eq!(settings::read(&c, 1).unwrap(), tighter);
    drop(c);
    std::fs::remove_dir_all(data).unwrap();
}

#[test]
fn the_cli_may_weaken_and_fills_missing_fields_with_defaults() {
    let data = crate::test_support::scratch("scrub-cli");
    let c = store::open(&data).unwrap();
    store::create_project(&c, "a").unwrap();
    settings::set(
        &c,
        1,
        &Policy {
            email: true,
            ..Policy::default()
        },
    )
    .unwrap();
    let mut out = Vec::new();
    let args = settings::Args {
        slug: "a".to_owned(),
        data: data.clone(),
        settings: Some(r#"{"ip":false}"#.to_owned()),
    };
    settings::run(&mut out, &args).unwrap();
    let expected = Policy {
        ip: false,
        ..Policy::default()
    };
    assert_eq!(serde_json::from_slice::<Policy>(&out).unwrap(), expected);
    assert_eq!(settings::read(&c, 1).unwrap(), expected);
    let unknown = settings::Args {
        settings: Some(r#"{"ips":false}"#.to_owned()),
        ..args
    };
    assert!(settings::run(&mut Vec::new(), &unknown).is_err());
    drop(c);
    std::fs::remove_dir_all(data).unwrap();
}

#[test]
fn a_stored_policy_with_fields_from_a_newer_version_still_reads() {
    let data = crate::test_support::scratch("scrub-stored");
    let c = store::open(&data).unwrap();
    store::create_project(&c, "a").unwrap();
    c.execute(
        r#"UPDATE projects SET scrub='{"ip":false,"phone":true,"extra_keys":["--","Tenant-Pin"]}'"#,
        [],
    )
    .unwrap();
    assert_eq!(
        settings::read(&c, 1).unwrap(),
        Policy {
            ip: false,
            extra_keys: HashSet::from(["tenantpin".to_owned()]),
            ..Policy::default()
        }
    );
    drop(c);
    std::fs::remove_dir_all(data).unwrap();
}

#[tokio::test]
async fn ingest_sees_an_api_change_at_once_and_a_cli_change_within_seconds() {
    let api = api();
    let db = Mutex::new(store::open(&api.data).unwrap());
    let start = Instant::now();
    assert_eq!(
        *api.policies.get_at(&db, 1, start).unwrap(),
        Policy::default()
    );
    let tighter = policy(&json!({"email": true}));
    assert_eq!(api.put(&tighter).await.0, 200);
    let after_api = api.policies.get_at(&db, 1, start).unwrap();
    assert!(after_api.email, "the API change applies at once");
    let cli = Policy::default();
    settings::set(&db.lock().unwrap(), 1, &cli).unwrap();
    assert!(api.policies.get_at(&db, 1, Instant::now()).unwrap().email);
    let later = Instant::now() + settings::FRESH;
    assert_eq!(*api.policies.get_at(&db, 1, later).unwrap(), cli);
    drop(db);
    api.finish();
}
