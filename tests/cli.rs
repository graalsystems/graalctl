//! End-to-end tests: run the real binary against an in-process mock of the
//! graal API and Keycloak.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
struct Request {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: String,
}

type Handler = dyn Fn(&Request) -> (u16, Vec<(&'static str, String)>, String) + Send + Sync;

struct Mock {
    url: String,
    requests: Arc<Mutex<Vec<Request>>>,
}

impl Mock {
    fn start(
        handler: impl Fn(&Request) -> (u16, Vec<(&'static str, String)>, String) + Send + Sync + 'static,
    ) -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let log = requests.clone();
        let handler: Arc<Handler> = Arc::new(handler);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() || line.is_empty() {
                    continue;
                }
                let mut parts = line.split_whitespace();
                let method = parts.next().unwrap_or_default().to_string();
                let path = parts.next().unwrap_or_default().to_string();
                let mut headers = HashMap::new();
                loop {
                    let mut h = String::new();
                    reader.read_line(&mut h).unwrap();
                    let h = h.trim_end();
                    if h.is_empty() {
                        break;
                    }
                    if let Some((k, v)) = h.split_once(':') {
                        headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                    }
                }
                let len: usize = headers
                    .get("content-length")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let mut body = vec![0; len];
                reader.read_exact(&mut body).unwrap();
                let req = Request {
                    method,
                    path,
                    headers,
                    body: String::from_utf8_lossy(&body).into_owned(),
                };
                let (status, extra, body) = handler(&req);
                log.lock().unwrap().push(req);
                let mut resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
                    body.len()
                );
                for (k, v) in extra {
                    resp.push_str(&format!("{k}: {v}\r\n"));
                }
                resp.push_str("\r\n");
                resp.push_str(&body);
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        Mock { url, requests }
    }

    fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}

fn graalctl(
    config_dir: &std::path::Path,
    env: &[(&str, &str)],
    args: &[&str],
    stdin: Option<&str>,
) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_graalctl"));
    cmd.args(args)
        .env("GRAALCTL_CONFIG_DIR", config_dir)
        .env_remove("GRAAL_TOKEN")
        .env_remove("GRAAL_CLIENT_ID")
        .env_remove("GRAAL_CLIENT_SECRET")
        .env_remove("GRAAL_API_URL")
        .env_remove("GRAAL_TENANT")
        .env_remove("GRAAL_CONTEXT")
        .env_remove("GRAAL_KEYCLOAK_URL")
        .env_remove("GRAAL_INFRASTRUCTURE")
        .env("NO_PROXY", "127.0.0.1")
        .env("no_proxy", "127.0.0.1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    drop(child.stdin.take());
    child.wait_with_output().unwrap()
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("graalctl-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn lists_projects_with_tenant_header_and_media_type() {
    let mock = Mock::start(|r| match r.path.as_str() {
        p if p.starts_with("/api/v1/projects") => (
            200,
            vec![],
            r#"{"content":[{"id":"p1","name":"Sales","description":"Sales analytics"}],"total":1}"#
                .into(),
        ),
        _ => (404, vec![], "{}".into()),
    });
    let dir = temp_dir("projects");
    let api = format!("{}/api/v1", mock.url);
    let env = [
        ("GRAAL_TOKEN", "t0k3n"),
        ("GRAAL_API_URL", api.as_str()),
        ("GRAAL_TENANT", "acme"),
    ];

    let out = graalctl(&dir, &env, &["projects", "list"], None);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "ID   NAME    DESCRIPTION       UPDATED\np1   Sales   Sales analytics   -\n"
    );

    let out = graalctl(&dir, &env, &["projects", "list", "-o", "id"], None);
    assert_eq!(stdout(&out), "p1\n");

    let req = &mock.requests()[0];
    assert_eq!(req.method, "GET");
    assert_eq!(req.path, "/api/v1/projects?page=0&size=100");
    assert_eq!(req.headers["authorization"], "Bearer t0k3n");
    assert_eq!(req.headers["x-tenant"], "acme");
    assert!(req.headers["accept"].starts_with("application/vnd.graal.systems.v1.project+json"));
}

#[test]
fn schedules_a_job_with_json_patch() {
    let mock = Mock::start(|_| {
        (
            200,
            vec![],
            r#"{"id":"j1","schedule":{"type":"cron","cron_expression":"0 6 * * *"}}"#.into(),
        )
    });
    let dir = temp_dir("schedule");
    let api = format!("{}/api/v1", mock.url);
    let env = [
        ("GRAAL_TOKEN", "t"),
        ("GRAAL_API_URL", api.as_str()),
        ("GRAAL_TENANT", "acme"),
    ];

    let out = graalctl(&dir, &env, &["jobs", "schedule", "j1", "0 6 * * *"], None);
    assert!(out.status.success(), "{}", stderr(&out));
    let req = &mock.requests()[0];
    assert_eq!(req.method, "PATCH");
    assert_eq!(req.path, "/api/v1/jobs/j1");
    assert_eq!(
        req.headers["content-type"],
        "application/json-patch+json;charset=UTF-8"
    );
    let body: serde_json::Value = serde_json::from_str(&req.body).unwrap();
    assert_eq!(
        body,
        serde_json::json!([{
            "op": "replace",
            "path": "/schedule",
            "value": {"type": "cron", "cron_expression": "0 6 * * *"}
        }])
    );
}

#[test]
fn runs_a_job_waits_and_streams_logs() {
    let polls = Arc::new(Mutex::new(0));
    let p = polls.clone();
    let mock = Mock::start(move |r| match (r.method.as_str(), r.path.as_str()) {
        ("GET", "/api/v1/infrastructures") => (200, vec![], r#"[{"id":"k8s-main"}]"#.into()),
        ("POST", "/api/v1/jobs/j1/runs") => {
            (201, vec![], r#"{"id":"r1","status":"queued"}"#.into())
        }
        ("GET", "/api/v1/jobs/j1/runs/r1/logs") => (
            200,
            vec![("X-Cursor", "c1".into())],
            r#"[{"message":"hello"}]"#.into(),
        ),
        ("GET", "/api/v1/jobs/j1/runs/r1/logs?cursor=c1") => {
            (200, vec![("X-Cursor", "c1".into())], "[]".into())
        }
        ("GET", "/api/v1/jobs/j1/runs/r1") => {
            let mut n = p.lock().unwrap();
            *n += 1;
            let status = if *n < 2 { "running" } else { "complete" };
            (200, vec![], format!(r#"{{"id":"r1","status":"{status}"}}"#))
        }
        _ => (
            404,
            vec![],
            r#"{"code":"JOB_NOT_FOUND","message":"no such job"}"#.into(),
        ),
    });
    let dir = temp_dir("run");
    let api = format!("{}/api/v1", mock.url);
    let env = [
        ("GRAAL_TOKEN", "t"),
        ("GRAAL_API_URL", api.as_str()),
        ("GRAAL_TENANT", "acme"),
    ];

    let out = graalctl(
        &dir,
        &env,
        &["jobs", "run", "j1", "--follow", "--interval", "1"],
        None,
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "hello\n");
    assert!(stderr(&out).contains("status: complete"));

    let start = mock
        .requests()
        .into_iter()
        .find(|r| r.method == "POST")
        .unwrap();
    assert_eq!(
        start.headers["content-type"],
        "application/vnd.graal.systems.v1.jobrun+json;charset=UTF-8"
    );
    let body: serde_json::Value = serde_json::from_str(&start.body).unwrap();
    assert_eq!(body["infrastructure_id"], "k8s-main");
}

#[test]
fn failed_run_and_api_errors_set_the_exit_code() {
    let mock = Mock::start(|r| match (r.method.as_str(), r.path.as_str()) {
        ("POST", "/api/v1/jobs/j1/runs") => (201, vec![], r#"{"id":"r1"}"#.into()),
        ("GET", "/api/v1/jobs/j1/runs/r1") => {
            (200, vec![], r#"{"id":"r1","status":"failed"}"#.into())
        }
        _ => (
            501,
            vec![],
            r#"{"code":"feature_disabled","message":"dbt jobs are disabled"}"#.into(),
        ),
    });
    let dir = temp_dir("fail");
    let api = format!("{}/api/v1", mock.url);
    let env = [
        ("GRAAL_TOKEN", "t"),
        ("GRAAL_API_URL", api.as_str()),
        ("GRAAL_TENANT", "acme"),
    ];

    let out = graalctl(
        &dir,
        &env,
        &["jobs", "run", "j1", "--infrastructure", "k8s", "--wait"],
        None,
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("run finished with status failed"));

    let out = graalctl(&dir, &env, &["jobs", "get", "nope"], None);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("HTTP 501 feature_disabled: dbt jobs are disabled"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn application_login_then_whoami_and_logout() {
    // Payload: {"azp":"ci-bot","exp":4102444800}
    let token = "eyJhbGciOiJub25lIn0.eyJhenAiOiJjaS1ib3QiLCJleHAiOjQxMDI0NDQ4MDB9.sig";
    let token_body = format!(r#"{{"access_token":"{token}","expires_in":300}}"#);
    let mock = Mock::start(move |r| match r.path.as_str() {
        "/api/v1/tenants/acme?attribute=realm" => (200, vec![], r#"{"realm":"acme-realm"}"#.into()),
        "/realms/acme-realm/protocol/openid-connect/token" => (200, vec![], token_body.clone()),
        _ => (404, vec![], "{}".into()),
    });
    let dir = temp_dir("login");
    let api = format!("{}/api/v1", mock.url);

    let out = graalctl(
        &dir,
        &[],
        &[
            "config",
            "set-context",
            "prod",
            "--api-url",
            &api,
            "--auth-url",
            &mock.url,
            "--tenant",
            "acme",
        ],
        None,
    );
    assert!(out.status.success(), "{}", stderr(&out));

    let out = graalctl(
        &dir,
        &[],
        &["login", "--client-id", "ci-bot", "--client-secret-stdin"],
        Some("s3cret\n"),
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).contains("as ci-bot"), "{}", stderr(&out));

    let token_req = mock
        .requests()
        .into_iter()
        .find(|r| r.path.contains("openid-connect/token"))
        .unwrap();
    assert_eq!(
        token_req.body,
        "grant_type=client_credentials&client_id=ci-bot&client_secret=s3cret"
    );

    // The realm is remembered, the secret is stored with owner-only permissions.
    let config = std::fs::read_to_string(dir.join("config.toml")).unwrap();
    assert!(config.contains("realm = \"acme-realm\""), "{config}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join("credentials.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    let out = graalctl(&dir, &[], &["whoami", "-o", "json"], None);
    assert!(out.status.success(), "{}", stderr(&out));
    let info: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(info["user"], "ci-bot");
    assert_eq!(info["tenant"], "acme");

    let out = graalctl(&dir, &[], &["logout"], None);
    assert!(out.status.success());
    let out = graalctl(&dir, &[], &["projects", "list"], None);
    assert!(stderr(&out).contains("not logged in"), "{}", stderr(&out));
}

#[test]
fn uploads_a_library_as_multipart_and_prints_its_key() {
    let mock = Mock::start(|r| match (r.method.as_str(), r.path.as_str()) {
        ("POST", "/api/v1/libraries") => {
            (201, vec![], r#"[{"key":"k-123","name":"job.py"}]"#.into())
        }
        _ => (404, vec![], "{}".into()),
    });
    let dir = temp_dir("upload");
    let file = dir.join("job.py");
    std::fs::write(&file, "print('hello')\n").unwrap();
    let api = format!("{}/api/v1", mock.url);
    let env = [
        ("GRAAL_TOKEN", "t"),
        ("GRAAL_API_URL", api.as_str()),
        ("GRAAL_TENANT", "acme"),
    ];

    let out = graalctl(
        &dir,
        &env,
        &["libraries", "upload", file.to_str().unwrap()],
        None,
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "k-123\n");

    let req = &mock.requests()[0];
    assert!(req.headers["content-type"].starts_with("multipart/form-data; boundary="));
    assert!(req
        .body
        .contains("Content-Disposition: form-data; name=\"file\"; filename=\"job.py\""));
    assert!(req.body.contains("print('hello')"));
}
