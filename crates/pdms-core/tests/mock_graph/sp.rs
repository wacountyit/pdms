// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! SharePoint endpoints for the mock Graph (plan P3): site, drive, list
//! columns, folders, upload sessions, downloads (a 302 to a pre-authenticated
//! `/blob/…` URL), moves, and list item fields. Upload and download URLs are
//! served through the hooks in `mod.rs`.

use std::collections::{BTreeMap, HashMap};

use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use pdms_core::sharepoint::quickxor::QuickXorHash;
use serde_json::{Map, Value, json};

use super::{MockGraph, MockState, graph_error, json_resp};

pub const SITE_URL: &str = "https://contoso.sharepoint.com/sites/PDMSTest";
pub const SITE: &str = "site-1";
pub const DRIVE: &str = "drive-1";
pub const LIST: &str = "list-1";

#[derive(Clone, Debug)]
pub struct SpItem {
    pub id: String,
    pub name: String,
    pub parent: Option<String>,
    pub folder: bool,
    pub content: Vec<u8>,
    pub fields: Map<String, Value>,
    pub list_item: String,
}

struct Session {
    parent: String,
    name: String,
    data: Vec<u8>,
}

#[derive(Default)]
pub struct SpState {
    items: BTreeMap<String, SpItem>,
    columns: Vec<String>,
    sessions: HashMap<String, Session>,
    next: u64,
    /// While set, SharePoint requests whose "METHOD path" (path without
    /// `/v1.0`) starts with this prefix get HTTP 500.
    fail_prefix: Option<String>,
}

impl SpState {
    fn ensure_root(&mut self) {
        self.items.entry("root".into()).or_insert_with(|| SpItem {
            id: "root".into(),
            name: String::new(),
            parent: None,
            folder: true,
            content: vec![],
            fields: Map::new(),
            list_item: "sp-li-root".into(),
        });
    }

    fn id(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("{prefix}-{}", self.next)
    }

    fn child(&self, parent: &str, name: &str) -> Option<&SpItem> {
        self.items
            .values()
            .find(|i| i.parent.as_deref() == Some(parent) && i.name.eq_ignore_ascii_case(name))
    }

    /// The item at a `/`-separated path from the library root.
    fn at(&self, path: &str) -> Option<&SpItem> {
        let mut current = self.items.get("root")?;
        for seg in path.split('/').filter(|s| !s.is_empty()) {
            current = self.child(&current.id, seg)?;
        }
        Some(current)
    }

    fn json(&self, i: &SpItem) -> Value {
        let mut v = json!({
            "id": i.id, "name": i.name, "size": i.content.len(),
            "parentReference": {"id": i.parent, "driveId": DRIVE},
            "listItem": {"id": i.list_item},
        });
        if i.folder {
            v["folder"] = json!({"childCount": 0});
        } else {
            let mut h = QuickXorHash::new();
            h.update(&i.content);
            v["file"] = json!({"hashes": {"quickXorHash": h.finish_base64()}});
        }
        v
    }

    fn insert(&mut self, parent: &str, name: String, folder: bool, content: Vec<u8>) -> Value {
        let id = self.id("sp-item");
        let list_item = self.id("sp-li");
        let item = SpItem {
            id: id.clone(),
            name,
            parent: Some(parent.to_owned()),
            folder,
            content,
            fields: Map::new(),
            list_item,
        };
        let out = self.json(&item);
        self.items.insert(id, item);
        out
    }
}

impl MockGraph {
    /// Names under a folder path ("" is the library root), sorted.
    pub fn names(&self, path: &str) -> Vec<String> {
        let mut s = self.state.lock().unwrap();
        s.sp.ensure_root();
        let Some(parent) = s.sp.at(path).map(|i| i.id.clone()) else {
            return vec![];
        };
        let mut v: Vec<String> =
            s.sp.items
                .values()
                .filter(|i| i.parent.as_deref() == Some(parent.as_str()))
                .map(|i| i.name.clone())
                .collect();
        v.sort();
        v
    }

    pub fn item(&self, path: &str) -> Option<SpItem> {
        let mut s = self.state.lock().unwrap();
        s.sp.ensure_root();
        s.sp.at(path).cloned()
    }

    pub fn fields(&self, path: &str) -> Map<String, Value> {
        self.item(path).map(|i| i.fields).unwrap_or_default()
    }

    pub fn columns(&self) -> Vec<String> {
        self.state.lock().unwrap().sp.columns.clone()
    }

    /// Change a file's bytes, as someone editing in SharePoint would.
    pub fn tamper(&self, path: &str, bytes: &[u8]) {
        let mut s = self.state.lock().unwrap();
        let id = s.sp.at(path).unwrap().id.clone();
        s.sp.items.get_mut(&id).unwrap().content = bytes.to_vec();
    }

    /// Fail every SharePoint request whose "METHOD path" starts with `prefix`
    /// (for example "POST /drives/drive-1/items/") until [`heal`](Self::heal).
    pub fn fail(&self, prefix: &str) {
        self.state.lock().unwrap().sp.fail_prefix = Some(prefix.to_owned());
    }

    pub fn heal(&self) {
        self.state.lock().unwrap().sp.fail_prefix = None;
    }
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// An authenticated `/v1.0` SharePoint request (`api` is the path after
/// `/v1.0`, without the query). `None` if the path isn't one of these.
pub(super) fn route(
    s: &mut MockState,
    base: &str,
    method: &Method,
    api: &str,
    body: &[u8],
) -> Option<Response> {
    let sp = &mut s.sp;
    sp.ensure_root();
    let line = format!("{method} {api}");
    let segs: Vec<&str> = api.trim_start_matches('/').split('/').collect();
    let ours =
        matches!(segs.first(), Some(&"drives")) || matches!(segs.get(..2), Some(["sites", _]));
    if !ours {
        return None;
    }
    if sp
        .fail_prefix
        .as_deref()
        .is_some_and(|p| line.starts_with(p))
    {
        return Some(graph_error(500, "generalException"));
    }
    let r = match (method.as_str(), segs.as_slice()) {
        ("GET", ["sites", host, "sites", name]) if host.ends_with(':') => {
            if format!("https://{}/sites/{name}", host.trim_end_matches(':')) == SITE_URL {
                json_resp(200, json!({"id": SITE}))
            } else {
                graph_error(404, "itemNotFound")
            }
        }
        ("GET", ["sites", SITE, "drives"]) => {
            json_resp(200, json!({"value": [{"id": DRIVE, "name": "Documents"}]}))
        }
        ("GET", ["drives", DRIVE, "list"]) => json_resp(200, json!({"id": LIST})),
        ("GET", ["sites", SITE, "lists", LIST, "columns"]) => {
            let mut v = vec![json!({"name": "Title"})];
            v.extend(sp.columns.iter().map(|c| json!({"name": c})));
            json_resp(200, json!({"value": v}))
        }
        ("POST", ["sites", SITE, "lists", LIST, "columns"]) => {
            let v: Value = serde_json::from_slice(body).unwrap();
            let name = v["name"].as_str().unwrap().to_owned();
            sp.columns.push(name.clone());
            json_resp(201, json!({"name": name}))
        }
        ("PATCH", ["sites", SITE, "lists", LIST, "items", li, "fields"]) => {
            let v: Value = serde_json::from_slice(body).unwrap();
            match sp.items.values_mut().find(|i| i.list_item == *li) {
                Some(item) => {
                    for (k, val) in v.as_object().unwrap() {
                        item.fields.insert(k.clone(), val.clone());
                    }
                    json_resp(200, Value::Object(item.fields.clone()))
                }
                None => graph_error(404, "itemNotFound"),
            }
        }
        ("GET", ["drives", DRIVE, "root:", rest @ ..]) => {
            let p = rest.iter().map(|s| decode(s)).collect::<Vec<_>>().join("/");
            match sp.at(&p).cloned() {
                Some(i) => json_resp(200, sp.json(&i)),
                None => graph_error(404, "itemNotFound"),
            }
        }
        ("GET", ["drives", DRIVE, "items", parent, name]) if parent.ends_with(':') => {
            match sp
                .child(parent.trim_end_matches(':'), &decode(name))
                .cloned()
            {
                Some(i) => json_resp(200, sp.json(&i)),
                None => graph_error(404, "itemNotFound"),
            }
        }
        ("POST", ["drives", DRIVE, "root", "children"]) => create_folder(sp, "root", body),
        ("POST", ["drives", DRIVE, "items", parent, "children"]) => create_folder(sp, parent, body),
        (
            "POST",
            [
                "drives",
                DRIVE,
                "items",
                parent,
                name,
                "createUploadSession",
            ],
        ) if parent.ends_with(':') && name.ends_with(':') => {
            let sid = sp.id("sp-session");
            sp.sessions.insert(
                sid.clone(),
                Session {
                    parent: parent.trim_end_matches(':').to_owned(),
                    name: decode(name.trim_end_matches(':')),
                    data: Vec::new(),
                },
            );
            json_resp(
                200,
                json!({"uploadUrl": format!("{base}/upload/{sid}"), "expirationDateTime": "2099-01-01T00:00:00Z"}),
            )
        }
        ("GET", ["drives", DRIVE, "items", id]) => match sp.items.get(*id).cloned() {
            Some(i) => json_resp(200, sp.json(&i)),
            None => graph_error(404, "itemNotFound"),
        },
        ("GET", ["drives", DRIVE, "items", id, "content"]) => {
            if sp.items.contains_key(*id) {
                let mut r = StatusCode::FOUND.into_response();
                r.headers_mut().insert(
                    header::LOCATION,
                    format!("{base}/blob/{id}").parse().unwrap(),
                );
                r
            } else {
                graph_error(404, "itemNotFound")
            }
        }
        ("PATCH", ["drives", DRIVE, "items", id]) => move_item(sp, id, body),
        ("DELETE", ["drives", DRIVE, "items", id]) => {
            sp.items.remove(*id);
            StatusCode::NO_CONTENT.into_response()
        }
        _ => return None,
    };
    Some(r)
}

/// A chunk PUT to `/upload/<session>`; `None` if it isn't a SharePoint session.
pub(super) fn upload(
    s: &mut MockState,
    session: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> Option<Response> {
    let sp = &mut s.sp;
    let current = sp.sessions.get_mut(session)?;
    let range = headers
        .get(header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let parsed = range.strip_prefix("bytes ").and_then(|r| {
        let (span, total) = r.split_once('/')?;
        let (start, _end) = span.split_once('-')?;
        Some((start.parse::<u64>().ok()?, total.parse::<u64>().ok()?))
    });
    let Some((start, total)) = parsed else {
        return Some(graph_error(400, "invalidRange"));
    };
    if start != current.data.len() as u64 {
        return Some(graph_error(416, "invalidRange"));
    }
    current.data.extend_from_slice(body);
    if (current.data.len() as u64) < total {
        let next = current.data.len();
        return Some(json_resp(
            202,
            json!({"nextExpectedRanges": [format!("{next}-")]}),
        ));
    }
    let done = sp.sessions.remove(session)?;
    if sp.child(&done.parent, &done.name).is_some() {
        return Some(graph_error(409, "nameAlreadyExists"));
    }
    Some(json_resp(
        201,
        sp.insert(&done.parent, done.name, false, done.data),
    ))
}

/// A pre-authenticated download `/blob/<item>`; `None` if not a SharePoint item.
pub(super) fn blob(s: &MockState, item: &str) -> Option<Response> {
    let i = s.sp.items.get(item)?;
    Some((StatusCode::OK, i.content.clone()).into_response())
}

fn create_folder(sp: &mut SpState, parent: &str, body: &[u8]) -> Response {
    let v: Value = serde_json::from_slice(body).unwrap();
    let name = v["name"].as_str().unwrap().to_owned();
    if !sp.items.contains_key(parent) {
        return graph_error(404, "itemNotFound");
    }
    if sp.child(parent, &name).is_some() {
        return graph_error(409, "nameAlreadyExists");
    }
    json_resp(201, sp.insert(parent, name, true, vec![]))
}

fn move_item(sp: &mut SpState, id: &str, body: &[u8]) -> Response {
    let v: Value = serde_json::from_slice(body).unwrap();
    let Some(current) = sp.items.get(id).cloned() else {
        return graph_error(404, "itemNotFound");
    };
    let parent = v["parentReference"]["id"]
        .as_str()
        .map(str::to_owned)
        .or(current.parent.clone())
        .unwrap();
    let name = v["name"]
        .as_str()
        .map_or(current.name.clone(), str::to_owned);
    if sp.child(&parent, &name).is_some_and(|other| other.id != id) {
        return graph_error(409, "nameAlreadyExists");
    }
    let item = sp.items.get_mut(id).unwrap();
    item.parent = Some(parent);
    item.name = name;
    let item = item.clone();
    json_resp(200, sp.json(&item))
}
