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

use super::{CLIENT, MockGraph, MockState, graph_error, json_resp};

pub const SITE_URL: &str = "https://contoso.sharepoint.com/sites/PDMSTest";
pub const SITE: &str = "site-1";
pub const DRIVE: &str = "drive-1";
pub const LIST: &str = "list-1";

/// The PDMS Service app's grant on the site (SHAREPOINT_SETUP.md B5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SiteGrant {
    /// No grant: Microsoft finds the site but lists no libraries.
    None,
    /// `write`: files can be read, but not the site's permissions.
    Write,
    #[default]
    FullControl,
}

#[derive(Clone, Debug, Default)]
pub struct SpItem {
    pub id: String,
    pub name: String,
    pub parent: Option<String>,
    pub folder: bool,
    pub content: Vec<u8>,
    pub fields: Map<String, Value>,
    pub list_item: String,
    /// Earlier contents, oldest first: SharePoint's version history. `content` is current.
    pub history: Vec<Vec<u8>>,
    /// Who created / last changed it: `None` is PDMS's app, `Some(name)` a person.
    pub created_by: Option<String>,
    pub modified_by: Option<String>,
    /// Direct permissions: (permission id, user email, SharePoint group name).
    pub perms: Vec<(String, Option<String>, Option<String>)>,
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
    /// Deleted items (the site recycle bin), by id.
    recycle: BTreeMap<String, SpItem>,
    /// The change feed: (sequence number, item id).
    changes: Vec<(u64, String)>,
    seq: u64,
    /// The next delta request that carries a token gets HTTP 410.
    expire_delta: bool,
    /// Every sharing invitation, as "invite <email> requireSignIn=… sendInvitation=… roles=…".
    invites: Vec<String>,
    grant: SiteGrant,
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
            ..Default::default()
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

    fn touch(&mut self, id: &str) {
        self.seq += 1;
        let seq = self.seq;
        self.changes.push((seq, id.to_owned()));
    }

    /// Graph's `parentReference.path`: `/drives/{id}/root:` plus the folders above.
    fn parent_path(&self, i: &SpItem) -> String {
        let mut names = Vec::new();
        let mut cur = i.parent.clone();
        while let Some(p) = cur {
            match self.items.get(&p) {
                Some(it) if it.id != "root" => {
                    names.push(it.name.clone());
                    cur = it.parent.clone();
                }
                _ => break,
            }
        }
        names.reverse();
        let mut s = format!("/drives/{DRIVE}/root:");
        for n in names {
            s.push('/');
            s.push_str(&n);
        }
        s
    }

    fn json(&self, i: &SpItem) -> Value {
        let mut v = json!({
            "id": i.id, "name": i.name, "size": i.content.len(),
            "parentReference": {"id": i.parent, "driveId": DRIVE, "path": self.parent_path(i)},
            "listItem": {"id": i.list_item},
            "createdBy": identity(i.created_by.as_deref()),
            "lastModifiedBy": identity(i.modified_by.as_deref()),
        });
        if i.id == "root" {
            v["root"] = json!({});
        }
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
            ..Default::default()
        };
        let out = self.json(&item);
        self.touch(&id);
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

    /// Someone edits a file in SharePoint: a new version, by that person.
    pub fn human_edit(&self, id: &str, bytes: &[u8], who: &str) {
        let mut s = self.state.lock().unwrap();
        let item = s.sp.items.get_mut(id).unwrap();
        let old = std::mem::replace(&mut item.content, bytes.to_vec());
        item.history.push(old);
        item.modified_by = Some(who.to_owned());
        s.sp.touch(id);
    }

    /// Someone renames a file or folder in SharePoint.
    pub fn human_rename(&self, id: &str, name: &str, who: &str) {
        let mut s = self.state.lock().unwrap();
        let item = s.sp.items.get_mut(id).unwrap();
        item.name = name.to_owned();
        item.modified_by = Some(who.to_owned());
        s.sp.touch(id);
    }

    /// Someone deletes an item; it goes to the recycle bin.
    pub fn human_delete(&self, id: &str, who: &str) {
        let mut s = self.state.lock().unwrap();
        let mut item = s.sp.items.remove(id).unwrap();
        item.modified_by = Some(who.to_owned());
        s.sp.recycle.insert(id.to_owned(), item);
        s.sp.touch(id);
    }

    /// A site owner restores an item from the recycle bin (same id).
    pub fn restore_from_recycle_bin(&self, id: &str) {
        let mut s = self.state.lock().unwrap();
        let item = s.sp.recycle.remove(id).unwrap();
        s.sp.items.insert(id.to_owned(), item);
        s.sp.touch(id);
    }

    /// Someone uploads their own file into a folder path. Returns its id.
    pub fn human_upload(&self, folder: &str, name: &str, bytes: &[u8], who: &str) -> String {
        let mut s = self.state.lock().unwrap();
        s.sp.ensure_root();
        let parent = s.sp.at(folder).unwrap().id.clone();
        let v = s.sp.insert(&parent, name.to_owned(), false, bytes.to_vec());
        let id = v["id"].as_str().unwrap().to_owned();
        let item = s.sp.items.get_mut(&id).unwrap();
        item.created_by = Some(who.to_owned());
        item.modified_by = Some(who.to_owned());
        id
    }

    /// A file PDMS's app put there whose save never committed. Returns its id.
    pub fn app_upload(&self, folder: &str, name: &str, bytes: &[u8]) -> String {
        let mut s = self.state.lock().unwrap();
        s.sp.ensure_root();
        let parent = s.sp.at(folder).unwrap().id.clone();
        let v = s.sp.insert(&parent, name.to_owned(), false, bytes.to_vec());
        v["id"].as_str().unwrap().to_owned()
    }

    /// A permission someone added by hand (a SharePoint group with edit rights).
    pub fn add_group_permission(&self, id: &str, group: &str) {
        let mut s = self.state.lock().unwrap();
        let pid = s.sp.id("sp-perm");
        s.sp.items
            .get_mut(id)
            .unwrap()
            .perms
            .push((pid, None, Some(group.to_owned())));
    }

    /// Emails with a direct permission on an item, sorted.
    pub fn readers(&self, id: &str) -> Vec<String> {
        let s = self.state.lock().unwrap();
        let mut v: Vec<String> = s.sp.items[id]
            .perms
            .iter()
            .filter_map(|p| p.1.clone())
            .collect();
        v.sort();
        v
    }

    pub fn content_of(&self, id: &str) -> Vec<u8> {
        self.state.lock().unwrap().sp.items[id].content.clone()
    }

    pub fn invites(&self) -> Vec<String> {
        self.state.lock().unwrap().sp.invites.clone()
    }

    pub fn expire_delta(&self) {
        self.state.lock().unwrap().sp.expire_delta = true;
    }

    /// Set the app's grant on the site (full control unless changed).
    pub fn site_grant(&self, grant: SiteGrant) {
        self.state.lock().unwrap().sp.grant = grant;
    }

    /// Refuse PDMS's sign-in (every API call gets HTTP 401) until called with `false`.
    pub fn deny(&self, on: bool) {
        self.state.lock().unwrap().min_token = if on { usize::MAX } else { 0 };
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
            if sp.grant == SiteGrant::None {
                json_resp(200, json!({"value": []}))
            } else {
                json_resp(200, json!({"value": [{"id": DRIVE, "name": "Documents"}]}))
            }
        }
        ("GET", ["sites", SITE, "permissions"]) => {
            if sp.grant == SiteGrant::FullControl {
                json_resp(
                    200,
                    json!({"value": [{"id": "perm-app", "roles": ["fullcontrol"],
                        "grantedToIdentitiesV2": [{"application": {"id": CLIENT}}]}]}),
                )
            } else {
                graph_error(403, "accessDenied")
            }
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
                    item.modified_by = None;
                    let (id, fields) = (item.id.clone(), item.fields.clone());
                    sp.touch(&id);
                    json_resp(200, Value::Object(fields))
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
            if let Some(mut item) = sp.items.remove(*id) {
                item.modified_by = None;
                sp.recycle.insert((*id).to_owned(), item);
                sp.touch(id);
            }
            StatusCode::NO_CONTENT.into_response()
        }
        ("GET", ["drives", DRIVE, "root"]) => {
            let root = sp.items["root"].clone();
            json_resp(200, sp.json(&root))
        }
        ("GET", ["drives", DRIVE, "root", "permissions"]) => {
            let v: Vec<Value> = sp.items["root"].perms.iter().map(perm_json).collect();
            json_resp(200, json!({"value": v}))
        }
        ("GET", ["drives", DRIVE, "root", "delta"]) => delta(sp, base, None),
        ("GET", ["drives", DRIVE, "root", "delta", token]) => delta(sp, base, token.parse().ok()),
        ("GET", ["drives", DRIVE, "items", id, "versions"]) => versions(sp, id),
        ("GET", ["drives", DRIVE, "items", id, "versions", v, "content"]) => {
            if sp.items.contains_key(*id) {
                let mut r = StatusCode::FOUND.into_response();
                r.headers_mut().insert(
                    header::LOCATION,
                    format!("{base}/blob/{id}~{v}").parse().unwrap(),
                );
                r
            } else {
                graph_error(404, "itemNotFound")
            }
        }
        (
            "POST",
            [
                "drives",
                DRIVE,
                "items",
                id,
                "versions",
                v,
                "restoreVersion",
            ],
        ) => restore_version(sp, id, v),
        ("POST", ["drives", DRIVE, "items", id, "invite"]) => invite(sp, id, body),
        ("GET", ["drives", DRIVE, "items", id, "permissions"]) => match sp.items.get(*id) {
            Some(i) => {
                let v: Vec<Value> = i.perms.iter().map(perm_json).collect();
                json_resp(200, json!({"value": v}))
            }
            None => graph_error(404, "itemNotFound"),
        },
        ("DELETE", ["drives", DRIVE, "items", id, "permissions", p]) => match sp.items.get_mut(*id)
        {
            Some(i) if i.perms.iter().any(|x| x.0 == *p) => {
                i.perms.retain(|x| x.0 != *p);
                StatusCode::NO_CONTENT.into_response()
            }
            _ => graph_error(404, "itemNotFound"),
        },
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

/// A pre-authenticated download `/blob/<item>` or `/blob/<item>~<version>`;
/// `None` if not a SharePoint item.
pub(super) fn blob(s: &MockState, item: &str) -> Option<Response> {
    let (id, version) = match item.split_once('~') {
        Some((i, v)) => (i, Some(v)),
        None => (item, None),
    };
    let i = s.sp.items.get(id)?;
    let content = match version {
        Some(v) => version_content(i, v)?,
        None => i.content.clone(),
    };
    Some((StatusCode::OK, content).into_response())
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
    item.modified_by = None;
    let item = item.clone();
    sp.touch(id);
    json_resp(200, sp.json(&item))
}

/// Graph's identity set for a change: PDMS's app (the mock's client id), or a person.
fn identity(person: Option<&str>) -> Value {
    match person {
        None => json!({
            "application": {"id": CLIENT, "displayName": "PDMS Service"},
            "user": {"displayName": "SharePoint App"}
        }),
        Some(n) => json!({"user": {
            "displayName": n,
            "email": format!("{}@county.example", n.to_lowercase().replace(' ', "."))
        }}),
    }
}

fn perm_json(p: &(String, Option<String>, Option<String>)) -> Value {
    match (&p.1, &p.2) {
        (Some(e), _) => {
            json!({"id": p.0, "roles": ["read"], "grantedToV2": {"user": {"email": e, "displayName": e}}})
        }
        (None, g) => {
            json!({"id": p.0, "roles": ["write"], "grantedToV2": {"siteGroup": {"displayName": g}}})
        }
    }
}

/// The change feed. The mock's delta link carries its token in the path.
fn delta(sp: &mut SpState, base: &str, token: Option<u64>) -> Response {
    if token.is_some() && sp.expire_delta {
        sp.expire_delta = false;
        return graph_error(410, "resyncChangesApplyDifferences");
    }
    let ids: Vec<String> = match token {
        None => sp
            .items
            .keys()
            .filter(|k| k.as_str() != "root")
            .cloned()
            .collect(),
        Some(t) => {
            let mut v: Vec<String> = Vec::new();
            for (_, id) in sp.changes.iter().filter(|(s, _)| *s > t) {
                v.retain(|x| x != id);
                v.push(id.clone());
            }
            v
        }
    };
    let value: Vec<Value> = ids
        .iter()
        .filter_map(|id| match sp.items.get(id) {
            Some(i) => {
                // The real feed leaves out paths.
                let mut v = sp.json(i);
                if let Some(p) = v["parentReference"].as_object_mut() {
                    p.remove("path");
                }
                Some(v)
            }
            None => sp.recycle.get(id).map(|i| {
                json!({"id": i.id, "deleted": {"state": "deleted"},
                       "lastModifiedBy": identity(i.modified_by.as_deref())})
            }),
        })
        .collect();
    let link = format!("{base}/v1.0/drives/{DRIVE}/root/delta/{}", sp.seq);
    json_resp(200, json!({"value": value, "@odata.deltaLink": link}))
}

fn versions(sp: &SpState, id: &str) -> Response {
    let Some(i) = sp.items.get(id) else {
        return graph_error(404, "itemNotFound");
    };
    let mut all: Vec<&Vec<u8>> = i.history.iter().collect();
    all.push(&i.content);
    // Newest first, as Graph lists them; version "N.0" is the Nth content.
    let value: Vec<Value> = all
        .iter()
        .enumerate()
        .rev()
        .map(|(n, c)| json!({"id": format!("{}.0", n + 1), "size": c.len()}))
        .collect();
    json_resp(200, json!({"value": value}))
}

/// The bytes of version "N.0" (1 is the oldest).
fn version_content(i: &SpItem, v: &str) -> Option<Vec<u8>> {
    let n: usize = v.split('.').next()?.parse().ok()?;
    if n == i.history.len() + 1 {
        Some(i.content.clone())
    } else {
        i.history.get(n.checked_sub(1)?).cloned()
    }
}

fn restore_version(sp: &mut SpState, id: &str, v: &str) -> Response {
    let Some(content) = sp.items.get(id).and_then(|i| version_content(i, v)) else {
        return graph_error(404, "itemNotFound");
    };
    let item = sp.items.get_mut(id).unwrap();
    let old = std::mem::replace(&mut item.content, content);
    item.history.push(old);
    item.modified_by = None;
    sp.touch(id);
    StatusCode::NO_CONTENT.into_response()
}

fn invite(sp: &mut SpState, id: &str, body: &[u8]) -> Response {
    let v: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let email = v["recipients"][0]["email"]
        .as_str()
        .unwrap_or("")
        .to_lowercase();
    sp.invites.push(format!(
        "invite {email} requireSignIn={} sendInvitation={} roles={}",
        v["requireSignIn"], v["sendInvitation"], v["roles"]
    ));
    if !sp.items.contains_key(id) {
        return graph_error(404, "itemNotFound");
    }
    if email.is_empty() || email.ends_with("@nobody.example") {
        return graph_error(400, "invalidRequest");
    }
    let fresh = sp.id("sp-perm");
    let item = sp.items.get_mut(id).unwrap();
    let existing = item
        .perms
        .iter()
        .find(|p| p.1.as_deref() == Some(email.as_str()))
        .map(|p| p.0.clone());
    let pid = match existing {
        Some(p) => p,
        None => {
            item.perms.push((fresh.clone(), Some(email.clone()), None));
            fresh
        }
    };
    json_resp(
        200,
        json!({"value": [perm_json(&(pid, Some(email), None))]}),
    )
}
