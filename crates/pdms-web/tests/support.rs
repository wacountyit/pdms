#![allow(
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
#![allow(dead_code)]

use std::collections::HashMap;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use http_body_util::BodyExt;
use pdms_core::auth::totp;
use pdms_core::config::Secret;
use pdms_core::{Config, Core, Ctx, directory};
use pdms_testkit::TestDb;
use tower::ServiceExt;
use uuid::Uuid;

pub const PW: &str = "correct horse battery staple";

pub fn config() -> Config {
    let mut c = Config::default();
    c.secret_key = Secret::new("dGVzdC1rZXktdGVzdC1rZXktdGVzdC1rZXktdGVzdC1rZXktdGVzdC1rZXk=");
    c.server.secure_cookies = false;
    c.org.name = "Test Org PDMS".into();
    let dir = std::env::temp_dir().join(format!("pdms-webtest-{}", Uuid::new_v4()));
    c.storage.local_path = dir.join("blobs");
    c.storage.spool_path = dir.join("spool");
    c
}

pub struct App {
    pub core: Core,
    pub router: Router,
    pub _db: TestDb,
}

pub async fn app_with(config: Config) -> App {
    let db = pdms_testkit::test_db().await;
    let core = Core::new(db.app.clone(), config).unwrap();
    let router = pdms_web::router(pdms_web::AppState::new(core.clone(), None, None));
    App {
        core,
        router,
        _db: db,
    }
}

pub async fn app() -> App {
    app_with(config()).await
}

pub fn system() -> Ctx {
    Ctx::system("test")
}

/// Create a user with local credentials; returns (user_id, totp secret).
pub async fn user(
    core: &Core,
    email: &str,
    dept: Option<Uuid>,
    role: Option<&str>,
) -> (Uuid, Vec<u8>) {
    let id = directory::create_user(&core.db, &system(), email, email, dept)
        .await
        .unwrap();
    let secret = totp::generate_secret();
    directory::set_local_credentials(&core.db, &core.keys, &system(), id, PW, Some(&secret))
        .await
        .unwrap();
    if let Some(key) = role {
        let r = directory::role_id_by_key(&core.db, key).await.unwrap();
        directory::assign_role(&core.db, &system(), id, r, None)
            .await
            .unwrap();
    }
    (id, secret)
}

pub struct Resp {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: String,
}

impl Resp {
    pub fn location(&self) -> &str {
        self.headers
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
    }

    /// The first `_csrf` hidden field on the page.
    pub fn csrf(&self) -> String {
        let re = regex::Regex::new(r#"name="_csrf" value="([^"]+)""#).unwrap();
        re.captures(&self.body)
            .map(|c| c[1].to_owned())
            .unwrap_or_default()
    }
}

/// A minimal browser: keeps cookies, does not follow redirects.
pub struct Browser {
    router: Router,
    cookies: HashMap<String, String>,
}

impl Browser {
    pub fn new(app: &App) -> Self {
        Browser {
            router: app.router.clone(),
            cookies: HashMap::new(),
        }
    }

    pub async fn send(&mut self, mut req: Request<Body>) -> Resp {
        if !self.cookies.is_empty() {
            let cookie = self
                .cookies
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; ");
            req.headers_mut()
                .insert(header::COOKIE, cookie.parse().unwrap());
        }
        let res = self.router.clone().oneshot(req).await.unwrap();
        for sc in res.headers().get_all(header::SET_COOKIE) {
            let sc = sc.to_str().unwrap();
            let (pair, attrs) = sc.split_once(';').unwrap_or((sc, ""));
            let (name, value) = pair.split_once('=').unwrap();
            if attrs.contains("Max-Age=0") || value.is_empty() {
                self.cookies.remove(name.trim());
            } else {
                self.cookies
                    .insert(name.trim().to_owned(), value.to_owned());
            }
        }
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        Resp {
            status,
            headers,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        }
    }

    pub async fn get(&mut self, path: &str) -> Resp {
        self.send(Request::get(path).body(Body::empty()).unwrap())
            .await
    }

    /// A multipart POST as a browser sends it: fields in order, optional file part.
    pub async fn multipart(
        &mut self,
        path: &str,
        fields: &[(&str, &str)],
        file: Option<(&str, &[u8])>,
    ) -> Resp {
        let boundary = "----pdmsTestBoundary7MA4YWxkTrZu0gW";
        let mut body: Vec<u8> = Vec::new();
        for (k, v) in fields {
            body.extend_from_slice(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n"
                )
                .as_bytes(),
            );
        }
        if let Some((name, bytes)) = file {
            body.extend_from_slice(
                format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes(),
            );
            body.extend_from_slice(bytes);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        let req = Request::post(path)
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .unwrap();
        self.send(req).await
    }

    pub async fn post(&mut self, path: &str, form: &[(&str, &str)]) -> Resp {
        self.post_with(path, form, &[]).await
    }

    pub async fn post_with(
        &mut self,
        path: &str,
        form: &[(&str, &str)],
        headers: &[(&str, &str)],
    ) -> Resp {
        let body = serde_urlencoded::to_string(form).unwrap();
        let mut b =
            Request::post(path).header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        self.send(b.body(Body::from(body)).unwrap()).await
    }

    pub async fn login(&mut self, email: &str, secret: &[u8]) -> Resp {
        let page = self.get("/login").await;
        let code = totp::current_code(secret, chrono::Utc::now().timestamp() as u64).unwrap();
        self.post(
            "/login/local",
            &[
                ("_csrf", &page.csrf()),
                ("username", email),
                ("password", PW),
                ("code", &code),
                ("next", "/"),
            ],
        )
        .await
    }

    pub fn has_session(&self) -> bool {
        self.cookies.contains_key("pdms_session")
    }
}

pub async fn actions(core: &Core) -> Vec<String> {
    sqlx::query_scalar("SELECT action FROM audit_log ORDER BY seq")
        .fetch_all(&core.db)
        .await
        .unwrap()
}
