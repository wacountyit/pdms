// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! PDMS web layer: Axum router, middleware, and server-rendered pages.

pub mod charts;
pub mod dashboard;
pub mod error;
pub mod middleware;
pub mod next_date;
pub mod routes;
pub mod seats;
pub mod session;
pub mod state;
pub mod timeline;
pub mod upload;
pub mod views;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::extract::{Request, State};
use axum::middleware::{Next, from_fn, from_fn_with_state};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use pdms_authz::Capability;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::TraceLayer;

pub use state::AppState;

use crate::session::CurrentUser;

/// Every `/admin` route requires the directory-management capability.
async fn require_admin(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let path = req
        .extensions()
        .get::<axum::extract::OriginalUri>()
        .map_or_else(|| req.uri().path().to_owned(), |u| u.0.path().to_owned());
    let (mut parts, body) = req.into_parts();
    let user = match <CurrentUser as axum::extract::FromRequestParts<AppState>>::from_request_parts(
        &mut parts, &state,
    )
    .await
    {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = user
        .require(&state, Capability::ManageDirectory, &path)
        .await
    {
        return e.into_response();
    }
    next.run(Request::from_parts(parts, body)).await
}

pub fn router(state: AppState) -> Router {
    {
        let b = state.branding();
        error::set_branding(&b.org_name, &b.version);
    }
    use routes::{
        admin, api, appointments, audit, auth, branding, collab, doc_admin, documents, exports,
        integrations, microsoft365, ops, schedule, sharepoint_admin, sign_in, system_settings,
        workflow,
    };

    let admin_routes = Router::new()
        .route("/users", get(admin::users).post(admin::create_user))
        .route(
            "/users/{id}",
            get(admin::user_detail).post(admin::update_user),
        )
        .route("/users/{id}/username", post(admin::set_username))
        .route("/users/{id}/status", post(admin::set_status))
        .route("/users/{id}/departments", post(admin::add_membership))
        .route(
            "/users/{id}/departments/{dept}/remove",
            post(admin::remove_membership),
        )
        .route("/users/{id}/roles", post(admin::assign_role))
        .route(
            "/users/{id}/local-credentials/remove",
            post(admin::remove_local_credentials),
        )
        .route("/role-assignments/{id}/revoke", post(admin::revoke_role))
        .route(
            "/departments",
            get(admin::departments).post(admin::create_department),
        )
        .route(
            "/departments/{id}",
            get(admin::department_detail).post(admin::update_department),
        )
        .route("/groups", get(admin::groups).post(admin::create_group))
        .route("/groups/{id}", get(admin::group_detail))
        .route("/groups/{id}/members", post(admin::add_group_member))
        .route(
            "/groups/{id}/members/{user}/remove",
            post(admin::remove_group_member),
        )
        .route("/roles", get(admin::roles))
        .route("/roles/{id}", post(admin::update_role))
        .route("/grants", get(admin::grants).post(admin::create_grant))
        .route("/grants/{id}/revoke", post(admin::revoke_grant))
        .route("/idp", get(admin::idp).post(admin::create_mapping))
        .route("/idp/{id}/delete", post(admin::delete_mapping))
        .route("/entra", get(admin::entra))
        .route("/entra/values", post(admin::add_entra_value))
        .route("/entra/values/remove", post(admin::remove_entra_value))
        .route("/entra/sync", post(admin::entra_sync_now))
        .route("/entra/settings", post(admin::save_entra_settings))
        .route("/entra/preview", get(admin::entra_preview))
        .route("/entra/test-group", post(admin::entra_test_group))
        .route_layer(from_fn_with_state(state.clone(), require_admin));

    let max_body = state.core.config().server.max_body_bytes;
    let upload_limit = DefaultBodyLimit::max(
        usize::try_from(state.core.config().storage.max_upload_bytes)
            .unwrap_or(usize::MAX)
            .saturating_add(1024 * 1024),
    );

    let branding_routes = Router::new()
        .route("/branding", get(branding::show).post(branding::save))
        .route(
            "/branding/seal",
            post(branding::upload_seal).layer(upload_limit),
        )
        .route("/branding/seal/remove", post(branding::remove_seal))
        .route(
            "/microsoft-365",
            get(microsoft365::page).post(microsoft365::save),
        )
        .route("/microsoft-365/test", post(microsoft365::test))
        .route("/sign-in", get(sign_in::page).post(sign_in::save))
        .route("/sign-in/test", post(sign_in::test))
        .route(
            "/email",
            get(system_settings::email_page).post(system_settings::email_save),
        )
        .route("/email/test", post(system_settings::email_test))
        .route(
            "/virus-scanning",
            get(system_settings::scan_page).post(system_settings::scan_save),
        )
        .route("/virus-scanning/test", post(system_settings::scan_test))
        .route("/microsoft-365/certificates", post(microsoft365::generate))
        .route(
            "/microsoft-365/certificates/{id}/activate",
            post(microsoft365::activate),
        )
        .route(
            "/microsoft-365/certificates/{id}/retire",
            post(microsoft365::retire),
        )
        .route(
            "/microsoft-365/certificates/{file}",
            get(microsoft365::download),
        )
        .route(
            "/branding/reset",
            get(branding::reset_confirm).post(branding::reset),
        );

    let api_routes = Router::new()
        .route("/openapi.json", get(api::openapi_json))
        .route("/me", get(api::me))
        .route("/document-types", get(api::document_types))
        .route(
            "/documents",
            get(api::list_documents).post(api::create_document),
        )
        .route("/documents/{id}", get(api::get_document))
        .route("/documents/{id}/versions", get(api::versions))
        .route("/documents/{id}/comments", get(api::comments))
        .route("/documents/{id}/text", get(api::text))
        .route("/documents/{id}/file", get(api::current_file))
        .route(
            "/documents/{id}/versions/{version}/file",
            get(api::version_file),
        )
        .route("/documents/{id}/status", post(api::change_status))
        .route("/calendar", get(api::calendar_events))
        .route("/positions", get(api::positions))
        .route("/positions/{id}", get(api::position))
        .route("/reports", get(api::report_list))
        .route("/reports/{key}", get(api::report))
        .route("/audit", get(api::audit))
        .fallback(|| async { api::ApiError::from(pdms_core::CoreError::NotFound) })
        .route_layer(from_fn_with_state(state.clone(), api::guard));

    // Each handler here checks its own capability.
    let doc_admin_routes = Router::new()
        .route(
            "/document-types",
            get(doc_admin::types_page).post(doc_admin::create_type),
        )
        .route(
            "/document-types/{id}",
            get(doc_admin::type_page).post(doc_admin::update_type),
        )
        .route(
            "/fields",
            get(doc_admin::fields_page).post(doc_admin::create_field),
        )
        .route(
            "/fields/{id}",
            get(doc_admin::field_page).post(doc_admin::update_field),
        )
        .route("/fields/{id}/deprecate", post(doc_admin::deprecate_field))
        .route(
            "/tags",
            get(doc_admin::tags_page).post(doc_admin::create_tag),
        )
        .route("/tags/{id}", post(doc_admin::update_tag))
        .route(
            "/retention",
            get(doc_admin::retention_page).post(doc_admin::create_policy),
        )
        .route("/retention/{id}", post(doc_admin::update_policy))
        .route("/deleted", get(doc_admin::deleted_page))
        .route("/deleted/{id}/restore", post(doc_admin::restore_deleted))
        .route(
            "/notification-rules",
            get(schedule::admin_rules).post(schedule::create_admin_rule),
        )
        .route("/notification-rules/{id}", post(schedule::update_rule))
        .route(
            "/notification-rules/{id}/delete",
            post(schedule::delete_rule),
        )
        .route("/notifications", get(schedule::deliveries))
        .route("/api", get(integrations::admin_api))
        .route(
            "/api/tokens/{id}/revoke",
            post(integrations::admin_revoke_token),
        )
        .route(
            "/webhooks",
            get(integrations::webhooks_page).post(integrations::create_webhook),
        )
        .route(
            "/webhooks/{id}",
            get(integrations::webhook_page).post(integrations::update_webhook),
        )
        .route(
            "/ocr",
            get(collab::ocr_page).post(system_settings::ocr_save),
        )
        .route("/ocr/test", post(system_settings::ocr_test))
        // The Outlook calendar sync was removed (D-087); its page's OCR part
        // moved to /admin/ocr.
        .route(
            "/outlook",
            get(|| async { Redirect::permanent("/admin/ocr") }),
        )
        .route("/webhooks/{id}/rotate", post(integrations::rotate_webhook))
        .route(
            "/webhooks/deliveries/{id}/redeliver",
            post(integrations::redeliver),
        )
        .route("/sharepoint", get(sharepoint_admin::page))
        .route("/sharepoint/check", post(sharepoint_admin::check_now))
        .route(
            "/sharepoint/settings",
            post(sharepoint_admin::save_settings),
        )
        .route("/sharepoint/test", post(sharepoint_admin::test))
        .route("/sharepoint/turn-on", post(sharepoint_admin::turn_on))
        .route("/sharepoint/turn-off", post(sharepoint_admin::turn_off))
        .route("/sharepoint/access", post(sharepoint_admin::update_access))
        .route(
            "/sharepoint/files/{version}/restore",
            post(sharepoint_admin::restore),
        )
        .route(
            "/sharepoint/files/{version}/accept",
            post(sharepoint_admin::accept),
        )
        .route(
            "/sharepoint/files/{version}/recheck",
            post(sharepoint_admin::recheck),
        )
        .route(
            "/sharepoint/unexpected/{item}/dismiss",
            post(sharepoint_admin::dismiss),
        );

    let document_routes = Router::new()
        .route("/new", get(documents::new_form))
        .route("/fields", get(documents::fields_partial))
        .route("/export.csv", get(documents::export_csv))
        .route("/{id}", get(documents::show))
        .route(
            "/{id}/edit",
            get(documents::edit_form).post(documents::update),
        )
        .route("/{id}/status", post(documents::change_status))
        .route("/{id}/owner", post(documents::transfer_owner))
        .route("/{id}/visibility", post(documents::set_visibility))
        .route("/{id}/retention", post(documents::retention))
        .route("/{id}/tags", post(documents::add_tag))
        .route("/{id}/tags/{tag}/remove", post(documents::remove_tag))
        .route("/{id}/grants", post(documents::share))
        .route("/{id}/grants/{grant}/revoke", post(documents::unshare))
        .route("/{id}/delete", post(documents::delete))
        .route("/{id}/download", get(documents::download_current))
        .route(
            "/{id}/versions",
            post(documents::add_version).layer(upload_limit),
        )
        .route(
            "/{id}/versions/{n}/download",
            get(documents::download_version),
        )
        .route(
            "/{id}/versions/{n}/restore",
            post(documents::restore_version),
        )
        .route("/{id}/history", get(documents::timeline))
        .route("/{id}/comments", post(collab::post_comment))
        .route("/{id}/comments/{c}/edit", post(collab::edit_comment))
        .route("/{id}/comments/{c}/delete", post(collab::delete_comment))
        .route("/{id}/comments/{c}/history", get(collab::comment_history))
        .route(
            "/{id}/suggestions/{s}/accept",
            post(collab::accept_suggestion),
        )
        .route(
            "/{id}/suggestions/{s}/dismiss",
            post(collab::dismiss_suggestion),
        )
        .route("/{id}/text/reprocess", post(collab::reprocess))
        .route("/{id}/workflow", post(workflow::submit))
        .route("/{id}/workflow/steps/{step}", post(workflow::decide))
        .route("/{id}/workflow/{wf}/comment", post(workflow::comment))
        .route("/{id}/workflow/{wf}/cancel", post(workflow::cancel))
        .route("/{id}/signatories", post(workflow::add_signatory))
        .route(
            "/{id}/signatories/{s}/request",
            post(workflow::request_signature),
        )
        .route(
            "/{id}/signatories/{s}/remove",
            post(workflow::remove_signatory),
        )
        .route(
            "/{id}/signatories/{s}/declined",
            post(workflow::decline_signature),
        )
        .route(
            "/{id}/signatories/{s}/signed",
            post(workflow::record_signed).layer(upload_limit),
        )
        .route("/{id}/tasks/{task}/start", post(schedule::start_task))
        .route("/{id}/tasks/{task}/complete", post(schedule::complete_task))
        .route("/{id}/tasks/{task}/approve", post(schedule::approve_task))
        .route("/{id}/rules", post(schedule::create_document_rule))
        .route("/{id}/rules/{rule}", post(schedule::update_rule))
        .route("/{id}/rules/{rule}/delete", post(schedule::delete_rule));

    Router::new()
        .route("/", get(schedule::dashboard))
        .route("/calendar", get(schedule::calendar_page))
        .route(
            "/calendar/feeds",
            get(schedule::feeds).post(schedule::create_feed),
        )
        .route("/calendar/feeds/{id}/revoke", post(schedule::revoke_feed))
        .route("/ics/{file}", get(schedule::ics))
        .route(
            "/positions",
            get(appointments::list).post(appointments::create),
        )
        .route(
            "/positions/{id}",
            get(appointments::show).post(appointments::update),
        )
        .route(
            "/positions/{id}/requirements",
            post(appointments::add_requirement),
        )
        .route(
            "/positions/{id}/requirements/{req}",
            post(appointments::update_requirement),
        )
        .route("/positions/{id}/appointments", post(appointments::appoint))
        .route(
            "/positions/{id}/appointments/{a}/reappoint",
            post(appointments::reappoint),
        )
        .route(
            "/positions/{id}/appointments/{a}/replace",
            post(appointments::replace),
        )
        .route(
            "/positions/{id}/appointments/{a}/end",
            post(appointments::end),
        )
        .route(
            "/positions/{id}/appointments/{a}/edit",
            post(appointments::edit),
        )
        .route(
            "/positions/{id}/appointments/{a}/requirements/{req}",
            post(appointments::check),
        )
        .route(
            "/account/tokens",
            get(integrations::my_tokens).post(integrations::create_token),
        )
        .route(
            "/account/tokens/{id}/revoke",
            post(integrations::revoke_token),
        )
        .route("/account/appearance", post(integrations::set_appearance))
        .route("/exports", get(exports::page).post(exports::build))
        .route("/exports/{id}/download", get(exports::download))
        .route("/reports", get(exports::reports_index))
        .route("/reports/{key}", get(exports::report))
        .route("/notifications", get(schedule::inbox_page))
        .route("/notifications/ack-all", post(schedule::acknowledge_all))
        .route("/notifications/{id}/ack", post(schedule::acknowledge))
        .route("/login", get(auth::login_page))
        .route("/login/local", post(auth::login_local))
        .route("/auth/oidc/start", get(auth::oidc_start))
        .route(
            pdms_core::auth::oidc::CALLBACK_PATH,
            get(auth::oidc_callback),
        )
        .route("/logout", post(auth::logout))
        .nest(
            "/admin",
            admin_routes.merge(doc_admin_routes).merge(branding_routes),
        )
        .nest("/documents", document_routes)
        .route(
            "/documents",
            get(documents::list)
                .post(documents::create)
                .layer(upload_limit),
        )
        .route("/tag-colors.css", get(documents::tag_colors))
        // Public: the sign-in page uses them too.
        .route("/theme.css", get(ops::theme_css))
        .route("/branding/seal", get(ops::seal))
        .route("/favicon.ico", get(ops::favicon))
        .route(
            "/admin",
            get(|| async { axum::response::Redirect::to("/admin/users") }),
        )
        .route("/audit", get(audit::audit))
        .route("/audit/export.csv", get(audit::export_csv))
        .route("/audit/export.json", get(audit::export_json))
        .route("/audit/verify", post(audit::verify))
        .nest("/api/v1", api_routes)
        .route("/api/docs", get(api::docs_page))
        .route("/source", get(ops::source))
        .route("/license", get(ops::license))
        .route("/healthz", get(ops::healthz))
        .route("/readyz", get(ops::readyz))
        .route("/metrics", get(ops::metrics))
        .route("/static/{file}", get(ops::static_file))
        .route("/static/fonts/{file}", get(ops::static_font))
        .fallback(|| async { error::AppError::Core(pdms_core::CoreError::NotFound) })
        // Layers run bottom-up for requests: the last added runs first.
        .layer(from_fn_with_state(
            state.clone(),
            middleware::branding_refresh,
        ))
        .layer(from_fn_with_state(state.clone(), middleware::flash))
        .layer(from_fn_with_state(state.clone(), middleware::csrf_guard))
        .layer(from_fn_with_state(state.clone(), middleware::client_info))
        .layer(from_fn_with_state(
            state.clone(),
            middleware::security_headers,
        ))
        .layer(from_fn(middleware::http_metrics))
        .layer(DefaultBodyLimit::max(max_body))
        .layer(TraceLayer::new_for_http())
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .with_state(state)
}
