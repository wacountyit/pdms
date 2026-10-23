// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Where a document's files go in SharePoint and what they are called (spec §5).
//! Pure functions, unit-tested here.

use chrono::NaiveDate;
use pdms_authz::Visibility;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::documents::VersionKind;

/// SharePoint Online's limit on a decoded path, in characters.
pub const PATH_LIMIT: usize = 400;
pub const DEPARTMENT_MAX: usize = 60;
pub const FOLDER_MAX: usize = 100;
pub const FILE_MAX: usize = 120;
/// Longest document ID kept in a name (IDs are like `CON-2026-000012`).
pub const ID_MAX: usize = 40;
pub const LABEL_MAX: usize = 40;

/// The three top-level folders (D-084).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Area {
    Departments,
    CountyWide,
    Restricted,
}

impl Area {
    pub const ALL: [Area; 3] = [Area::Departments, Area::CountyWide, Area::Restricted];

    /// The folder's name in the library.
    pub fn folder_name(self) -> &'static str {
        match self {
            Area::Departments => "Departments",
            Area::CountyWide => "County-wide",
            Area::Restricted => "Restricted",
        }
    }

    /// Stored in `sharepoint_department_folders.area`.
    pub fn as_str(self) -> &'static str {
        match self {
            Area::Departments => "departments",
            Area::CountyWide => "county_wide",
            Area::Restricted => "restricted",
        }
    }
}

pub fn area_for(v: Visibility) -> Area {
    match v {
        Visibility::Department => Area::Departments,
        Visibility::CountyWide => Area::CountyWide,
        Visibility::Restricted => Area::Restricted,
    }
}

/// Where a document's folder belongs.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Spot {
    pub area: Area,
    pub department_id: Uuid,
}

impl Spot {
    /// Soft-deleted documents leave everyone's view: they wait in `Restricted`.
    pub fn of(visibility: Visibility, department_id: Uuid, deleted: bool) -> Spot {
        let area = if deleted {
            Area::Restricted
        } else {
            area_for(visibility)
        };
        Spot {
            area,
            department_id,
        }
    }
}

/// Whether moving a folder from `from` to `to` takes it away from someone who
/// can browse it now. Such moves happen before the change is saved (spec §7.3).
pub fn narrows(from: Spot, to: Spot) -> bool {
    if from == to {
        return false;
    }
    match (from.area, to.area) {
        (Area::Restricted, _) => false,
        (_, Area::Restricted) => true,
        (Area::CountyWide, Area::Departments) => true,
        (Area::Departments, Area::Departments) => from.department_id != to.department_id,
        _ => false,
    }
}

/// Make `s` safe as one SharePoint file or folder name, at most `max_chars`.
pub fn sanitize_segment(s: &str, max_chars: usize) -> String {
    let replaced: String = s
        .chars()
        .map(|c| match c {
            '"' | '*' | ':' | '<' | '>' | '?' | '/' | '\\' | '|' | '#' | '%' => '_',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let mut out = replaced.split_whitespace().collect::<Vec<_>>().join(" ");
    // SharePoint rejects `_vti_` anywhere in a name.
    while let Some(i) = out.to_ascii_lowercase().find("_vti_") {
        let keep = out[i..i + 4].to_owned();
        out.replace_range(i..i + 5, &format!("{keep}-"));
    }
    let cut: String = out.chars().take(max_chars).collect();
    let trimmed = cut
        .trim_start_matches(['~', ' ', '.'])
        .trim_end_matches([' ', '.']);
    if trimmed.is_empty() {
        "Untitled".to_owned()
    } else {
        trimmed.to_owned()
    }
}

pub fn department_folder_name(name: &str) -> String {
    sanitize_segment(name, DEPARTMENT_MAX)
}

/// Used when another department already has the plain name.
pub fn department_folder_name_with_code(name: &str, code: &str) -> String {
    let code = sanitize_segment(code, 32);
    let room = DEPARTMENT_MAX
        .saturating_sub(code.chars().count() + 3)
        .max(1);
    format!("{} ({code})", sanitize_segment(name, room))
}

pub fn document_folder_name(external_id: &str, title: &str) -> String {
    let id = sanitize_segment(external_id, ID_MAX);
    let room = FOLDER_MAX - id.chars().count() - 3;
    format!("{id} – {}", sanitize_segment(title, room))
}

pub fn file_name(external_id: &str, version_no: i32, label: &str, ext: &str) -> String {
    format!(
        "{} v{version_no} – {}.{ext}",
        sanitize_segment(external_id, ID_MAX),
        sanitize_segment(label, LABEL_MAX)
    )
}

/// The name a file carries between upload and the mirror job (spec §7.1).
pub fn staging_name(external_id: &str, token: &str, ext: &str) -> String {
    format!(
        "{} (uploading {token}).{ext}",
        sanitize_segment(external_id, ID_MAX)
    )
}

pub fn version_label(kind: VersionKind) -> &'static str {
    match kind {
        VersionKind::Initial => "Original",
        VersionKind::Revision => "Revision",
        VersionKind::Amendment => "Amendment",
        VersionKind::SignedCopy => "Signed copy",
        VersionKind::Restore => "Restored",
        VersionKind::Metadata => "File",
    }
}

/// The extension a stored file gets: from the type sniffed from its content
/// (D-027), so SharePoint never shows a script name for a text file.
pub fn extension_for(media_type: &str, file_name: &str) -> String {
    let known = match media_type {
        "application/pdf" => "pdf",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "pptx",
        "application/msword" => "doc",
        "application/vnd.ms-excel" => "xls",
        "application/vnd.ms-powerpoint" => "ppt",
        "application/vnd.ms-outlook" => "msg",
        "application/vnd.oasis.opendocument.text" => "odt",
        "application/vnd.oasis.opendocument.spreadsheet" => "ods",
        "application/vnd.oasis.opendocument.presentation" => "odp",
        "application/rtf" => "rtf",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/tiff" => "tif",
        "image/webp" => "webp",
        "text/plain" => "txt",
        "text/csv" => "csv",
        "message/rfc822" => "eml",
        _ => "",
    };
    if !known.is_empty() {
        return known.to_owned();
    }
    file_name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .filter(|e| !e.is_empty() && e.len() <= 10 && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or_else(|| "bin".to_owned())
}

// Column internal names (spec §5). `Title` is the library's built-in column.
pub const DOCUMENT_ID: &str = "PdmsDocumentId";
pub const DOCUMENT_TYPE: &str = "PdmsDocumentType";
pub const TAGS: &str = "PdmsTags";
pub const STATUS: &str = "PdmsStatus";
pub const VISIBILITY: &str = "PdmsVisibility";
pub const DEPARTMENT: &str = "PdmsDepartment";
pub const OWNER: &str = "PdmsOwner";
pub const EFFECTIVE_DATE: &str = "PdmsEffectiveDate";
pub const REVIEW_DATE: &str = "PdmsReviewDate";
pub const RENEWAL_DATE: &str = "PdmsRenewalDate";
pub const EXPIRATION_DATE: &str = "PdmsExpirationDate";
pub const VERSION: &str = "PdmsVersion";
pub const CURRENT: &str = "PdmsCurrent";
pub const LINK: &str = "PdmsLink";

/// What PDMS shows about a document, for the SharePoint columns.
pub struct PlacementDoc<'a> {
    pub external_id: &'a str,
    pub title: &'a str,
    pub type_name: &'a str,
    pub tags: &'a [String],
    pub status: &'a str,
    pub visibility: Visibility,
    pub department: &'a str,
    pub owner: &'a str,
    pub effective_date: Option<NaiveDate>,
    pub review_date: Option<NaiveDate>,
    pub renewal_date: Option<NaiveDate>,
    pub expiration_date: Option<NaiveDate>,
    pub link: &'a str,
}

pub struct PlacementFile {
    pub version_no: i32,
    /// The newest file of the document.
    pub current: bool,
}

/// SharePoint's single-line text columns, the built-in `Title` among them,
/// hold at most 255 characters.
pub const TEXT_MAX: usize = 255;

/// `s` cut to [`TEXT_MAX`] characters. PDMS titles can be 500 characters;
/// SharePoint refuses longer values, so they are truncated there (the owner's
/// decision, 2026-10-22). PDMS keeps the full value.
fn text_value(s: &str) -> Value {
    Value::String(s.chars().take(TEXT_MAX).collect())
}

/// Column values for one file. Dates are noon UTC, so every US time zone
/// shows the same day. Single-line text is capped at [`TEXT_MAX`].
pub fn columns(doc: &PlacementDoc<'_>, file: &PlacementFile) -> Map<String, Value> {
    let date =
        |d: Option<NaiveDate>| d.map_or(Value::Null, |d| Value::String(format!("{d}T12:00:00Z")));
    let mut m = Map::new();
    m.insert("Title".into(), text_value(doc.title));
    m.insert(DOCUMENT_ID.into(), text_value(doc.external_id));
    m.insert(DOCUMENT_TYPE.into(), text_value(doc.type_name));
    m.insert(TAGS.into(), json!(doc.tags.join("; ")));
    m.insert(STATUS.into(), text_value(doc.status));
    m.insert(VISIBILITY.into(), json!(doc.visibility.label()));
    m.insert(DEPARTMENT.into(), text_value(doc.department));
    m.insert(OWNER.into(), text_value(doc.owner));
    m.insert(EFFECTIVE_DATE.into(), date(doc.effective_date));
    m.insert(REVIEW_DATE.into(), date(doc.review_date));
    m.insert(RENEWAL_DATE.into(), date(doc.renewal_date));
    m.insert(EXPIRATION_DATE.into(), date(doc.expiration_date));
    m.insert(VERSION.into(), json!(file.version_no));
    m.insert(CURRENT.into(), json!(file.current));
    m.insert(LINK.into(), text_value(doc.link));
    m
}

/// Library columns PDMS creates when they are missing
/// (`POST /sites/{site}/lists/{list}/columns`).
pub fn column_definitions() -> Vec<Value> {
    let text = |name: &str, display: &str| json!({"name": name, "displayName": display, "text": {"allowMultipleLines": false, "maxLength": TEXT_MAX}});
    let date = |name: &str, display: &str, indexed: bool| json!({"name": name, "displayName": display, "indexed": indexed, "dateTime": {"format": "dateOnly"}});
    vec![
        text(DOCUMENT_ID, "Document ID"),
        text(DOCUMENT_TYPE, "Document Type"),
        json!({"name": TAGS, "displayName": "Tags", "text": {"allowMultipleLines": true}}),
        text(STATUS, "Status"),
        json!({"name": VISIBILITY, "displayName": "Visibility",
               "choice": {"choices": ["Department", "County-wide", "Restricted"], "displayAs": "dropDownMenu"}}),
        text(DEPARTMENT, "Department"),
        text(OWNER, "Owner"),
        date(EFFECTIVE_DATE, "Effective Date", false),
        date(REVIEW_DATE, "Review Date", true),
        date(RENEWAL_DATE, "Renewal Date", false),
        date(EXPIRATION_DATE, "Expiration Date", false),
        json!({"name": VERSION, "displayName": "Version", "number": {}}),
        json!({"name": CURRENT, "displayName": "Current", "indexed": true, "boolean": {}}),
        text(LINK, "Open in PDMS"),
    ]
}

/// `https://<tenant>.sharepoint.com/sites/<Name>` → (host, `/sites/<Name>`).
pub fn parse_site_url(s: &str) -> Result<(String, String), String> {
    let url = url::Url::parse(s.trim()).map_err(|_| {
        "enter the site's full address, e.g. https://contoso.sharepoint.com/sites/PDMSRecords"
            .to_owned()
    })?;
    if url.scheme() != "https" {
        return Err("the site address must start with https://".into());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "the site address has no host".to_owned())?
        .to_owned();
    let segs: Vec<&str> = url
        .path_segments()
        .map(|p| p.filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();
    match segs.as_slice() {
        [kind @ ("sites" | "teams"), name] => Ok((host, format!("/{kind}/{name}"))),
        _ => Err(
            "the site address must look like https://<tenant>.sharepoint.com/sites/<SiteName>"
                .into(),
        ),
    }
}

/// Percent-encode one path segment for Graph path addressing (RFC 3986:
/// everything except unreserved characters).
pub fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;
    use pdms_authz::Visibility;
    use serde_json::json;
    use uuid::Uuid;

    use super::*;
    use crate::documents::VersionKind;

    const A: Uuid = Uuid::from_u128(1);
    const B: Uuid = Uuid::from_u128(2);

    #[test]
    fn visibility_picks_the_area_and_deleted_documents_leave_view() {
        assert_eq!(area_for(Visibility::Department), Area::Departments);
        assert_eq!(area_for(Visibility::CountyWide), Area::CountyWide);
        assert_eq!(area_for(Visibility::Restricted), Area::Restricted);
        assert_eq!(
            Spot::of(Visibility::CountyWide, A, true).area,
            Area::Restricted
        );
        assert_eq!(
            Area::ALL.map(Area::folder_name),
            ["Departments", "County-wide", "Restricted"]
        );
    }

    #[test]
    fn narrowing_is_any_move_that_takes_the_document_from_someone() {
        let s = |area, d| Spot {
            area,
            department_id: d,
        };
        use Area::*;
        assert!(narrows(s(Departments, A), s(Restricted, A)));
        assert!(narrows(s(CountyWide, A), s(Restricted, A)));
        assert!(narrows(s(CountyWide, A), s(Departments, A)));
        assert!(narrows(s(Departments, A), s(Departments, B)));
        assert!(!narrows(s(Departments, A), s(CountyWide, A)));
        assert!(!narrows(s(Restricted, A), s(Departments, A)));
        assert!(!narrows(s(Restricted, A), s(Restricted, B)));
        assert!(!narrows(s(CountyWide, A), s(CountyWide, B)));
        assert!(!narrows(s(Departments, A), s(Departments, A)));
    }

    #[test]
    fn sanitize_replaces_forbidden_characters_and_trims() {
        assert_eq!(
            sanitize_segment(
                "Q3 \"Final\" Agreement: Road #5 / Bridge 50% *draft* <a|b>?\\",
                200
            ),
            "Q3 _Final_ Agreement_ Road _5 _ Bridge 50_ _draft_ _a_b___"
        );
        assert_eq!(sanitize_segment("~~ .hidden. ", 50), "hidden");
        assert_eq!(
            sanitize_segment("tab\there\nnewline", 50),
            "tab here newline"
        );
        assert_eq!(
            sanitize_segment("my_vti_file and _VTI_x", 50),
            "my_vti-file and _VTI-x"
        );
        assert_eq!(sanitize_segment(":::", 50), "___");
        assert_eq!(sanitize_segment("...", 50), "Untitled");
        assert_eq!(sanitize_segment("", 50), "Untitled");
    }

    #[test]
    fn sanitize_cuts_by_characters_not_bytes() {
        let s = sanitize_segment(&"é".repeat(300), 10);
        assert_eq!(s.chars().count(), 10);
        assert_eq!(sanitize_segment("abc def", 4), "abc");
    }

    #[test]
    fn names_keep_the_document_id_intact() {
        let long = "x".repeat(500);
        let folder = document_folder_name("CON-2026-000012", &long);
        assert!(folder.starts_with("CON-2026-000012 – x"));
        assert!(folder.chars().count() <= FOLDER_MAX);
        assert_eq!(
            document_folder_name("CON-2026-000012", "Fuel Supply Agreement"),
            "CON-2026-000012 – Fuel Supply Agreement"
        );
        assert_eq!(
            file_name(
                "CON-2026-000012",
                3,
                version_label(VersionKind::SignedCopy),
                "pdf"
            ),
            "CON-2026-000012 v3 – Signed copy.pdf"
        );
        assert_eq!(
            staging_name("CON-2026-000012", "1a2b3c4d", "pdf"),
            "CON-2026-000012 (uploading 1a2b3c4d).pdf"
        );
        assert_eq!(department_folder_name("Road & Bridge"), "Road & Bridge");
        assert_eq!(
            department_folder_name_with_code("Road:Bridge", "RB2"),
            "Road_Bridge (RB2)"
        );
        assert!(
            department_folder_name_with_code(&"d".repeat(200), "CODE")
                .chars()
                .count()
                <= DEPARTMENT_MAX
        );
    }

    #[test]
    fn paths_stay_under_the_sharepoint_limit() {
        // Worst case: an 80-character site path, the library, the longest area,
        // and every segment at its cap.
        let site_and_library = format!("/sites/{}/Shared Documents", "s".repeat(60));
        let dept = department_folder_name(&"d".repeat(500));
        let folder = document_folder_name(&"I".repeat(80), &"t".repeat(500));
        let file = file_name(&"I".repeat(80), i32::MAX, &"l".repeat(200), "docx");
        let path = format!("{site_and_library}/County-wide/{dept}/{folder}/{file}");
        assert!(file.chars().count() <= FILE_MAX, "{}", file.chars().count());
        assert!(
            path.chars().count() <= PATH_LIMIT,
            "{}",
            path.chars().count()
        );
    }

    #[test]
    fn labels_and_extensions() {
        assert_eq!(version_label(VersionKind::Initial), "Original");
        assert_eq!(version_label(VersionKind::Restore), "Restored");
        assert_eq!(extension_for("application/pdf", "whatever.bin"), "pdf");
        assert_eq!(
            extension_for(
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                "x"
            ),
            "docx"
        );
        // The sniffed type wins over the client's name (D-027).
        assert_eq!(extension_for("text/plain", "payload.bat"), "txt");
        assert_eq!(extension_for("application/unknown", "Report.ODF"), "odf");
        assert_eq!(extension_for("application/unknown", "no-extension"), "bin");
        assert_eq!(extension_for("application/unknown", "weird.ex e"), "bin");
    }

    #[test]
    fn columns_carry_pdms_metadata() {
        let tags = vec!["Roads".to_owned(), "FY2026".to_owned()];
        let doc = PlacementDoc {
            external_id: "CON-2026-000012",
            title: "Fuel Supply Agreement",
            type_name: "Contract",
            tags: &tags,
            status: "Active",
            visibility: Visibility::CountyWide,
            department: "Road and Bridge",
            owner: "Alice Ops",
            effective_date: NaiveDate::from_ymd_opt(2026, 1, 15),
            review_date: None,
            renewal_date: NaiveDate::from_ymd_opt(2027, 1, 14),
            expiration_date: None,
            link: "https://pdms.example.org/documents/abc",
        };
        let m = columns(
            &doc,
            &PlacementFile {
                version_no: 4,
                current: true,
            },
        );
        assert_eq!(m["Title"], json!("Fuel Supply Agreement"));
        assert_eq!(m[DOCUMENT_ID], json!("CON-2026-000012"));
        assert_eq!(m[TAGS], json!("Roads; FY2026"));
        assert_eq!(m[VISIBILITY], json!("County-wide"));
        assert_eq!(m[EFFECTIVE_DATE], json!("2026-01-15T12:00:00Z"));
        assert_eq!(m[REVIEW_DATE], json!(null));
        assert_eq!(m[VERSION], json!(4));
        assert_eq!(m[CURRENT], json!(true));
        assert_eq!(m[LINK], json!("https://pdms.example.org/documents/abc"));
        // Every column written (except the built-in Title) is provisioned, once.
        let defined: Vec<String> = column_definitions()
            .iter()
            .map(|d| d["name"].as_str().unwrap().to_owned())
            .collect();
        for key in m.keys().filter(|k| *k != "Title") {
            assert_eq!(defined.iter().filter(|d| *d == key).count(), 1, "{key}");
        }
    }

    #[test]
    fn long_text_is_cut_to_sharepoints_limit() {
        let title = "t".repeat(500);
        let doc = PlacementDoc {
            external_id: "CON-2026-000012",
            title: &title,
            type_name: "Contract",
            tags: &[],
            status: "Active",
            visibility: Visibility::Department,
            department: "Road and Bridge",
            owner: "Alice Ops",
            effective_date: None,
            review_date: None,
            renewal_date: None,
            expiration_date: None,
            link: "https://pdms.example.org/documents/abc",
        };
        let m = columns(
            &doc,
            &PlacementFile {
                version_no: 1,
                current: true,
            },
        );
        assert_eq!(m["Title"].as_str().unwrap().chars().count(), TEXT_MAX);
        assert_eq!(
            m[DEPARTMENT],
            json!("Road and Bridge"),
            "short values are untouched"
        );
    }

    #[test]
    fn site_urls() {
        assert_eq!(
            parse_site_url("https://contoso.sharepoint.com/sites/PDMSRecords/").unwrap(),
            (
                "contoso.sharepoint.com".to_owned(),
                "/sites/PDMSRecords".to_owned()
            )
        );
        assert!(parse_site_url("https://contoso.sharepoint.com/teams/PDMS").is_ok());
        assert!(parse_site_url("http://contoso.sharepoint.com/sites/PDMS").is_err());
        assert!(parse_site_url("https://contoso.sharepoint.com/").is_err());
        assert!(
            parse_site_url("https://contoso.sharepoint.com/sites/PDMS/Shared%20Documents").is_err()
        );
        assert!(parse_site_url("not a url").is_err());
    }

    #[test]
    fn path_segments_are_percent_encoded() {
        assert_eq!(encode_segment("Road and Bridge"), "Road%20and%20Bridge");
        assert_eq!(encode_segment("a#b%c"), "a%23b%25c");
        assert_eq!(encode_segment("CON-1 – x"), "CON-1%20%E2%80%93%20x");
        assert_eq!(encode_segment("safe-._~"), "safe-._~");
    }
}
