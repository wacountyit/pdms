// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
use crate::model::Principal;

/// Keys of the system roles seeded by migration 0001.
pub mod role_keys {
    pub const ADMIN: &str = "admin";
    pub const AUDITOR: &str = "auditor";
    pub const RECORDS_OFFICER: &str = "records_officer";
    pub const LEGAL_COUNSEL: &str = "legal_counsel";
    pub const DEPARTMENT_HEAD: &str = "department_head";
    /// Seeded by migration 0005.
    pub const APPOINTMENTS_CLERK: &str = "appointments_clerk";
}

/// Non-document capabilities, conferred only by org-wide system roles.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Capability {
    /// Users, departments, groups, roles, IdP mappings.
    ManageDirectory,
    /// Runtime settings.
    ManageSettings,
    ViewAudit,
    ExportAudit,
    VerifyAudit,
    /// Records request packages (still limited to documents the user can view).
    BuildExportPackages,
    /// Document types and custom field definitions.
    ManageDocumentSchema,
    /// Retention policies, per-document retention overrides, and legal holds.
    ManageRetention,
    /// Curated tags (names, colors); anyone who can edit a document can add free-form tags.
    ManageTags,
    /// See and restore soft-deleted documents.
    RestoreDeletedDocuments,
    /// Positions, appointments, and requirement checklists.
    ManageAppointments,
    /// Read every position and appointment (others see the positions they
    /// manage and those of their departments).
    ViewAllAppointments,
    /// Access review report: every grant, role, and orphaned account.
    ViewAccessReports,
    /// Webhooks, integration providers, and everyone's API tokens.
    ManageIntegrations,
    /// SharePoint status, department-folder access, and resolving files that
    /// were changed directly in SharePoint.
    ManageSharePoint,
}

impl Capability {
    fn granted_by(self) -> &'static [&'static str] {
        use role_keys::*;
        match self {
            Capability::ManageDirectory
            | Capability::ManageSettings
            | Capability::ManageDocumentSchema
            | Capability::ManageTags
            | Capability::RestoreDeletedDocuments
            | Capability::ManageSharePoint => &[ADMIN],
            Capability::ManageRetention => &[ADMIN, RECORDS_OFFICER],
            Capability::ViewAudit | Capability::VerifyAudit => &[ADMIN, AUDITOR],
            Capability::ExportAudit => &[AUDITOR],
            Capability::BuildExportPackages => &[RECORDS_OFFICER],
            Capability::ManageAppointments => &[ADMIN, APPOINTMENTS_CLERK],
            Capability::ViewAllAppointments => &[ADMIN, APPOINTMENTS_CLERK, AUDITOR],
            Capability::ViewAccessReports => &[ADMIN, AUDITOR],
            Capability::ManageIntegrations => &[ADMIN],
        }
    }
}

pub fn has_capability(principal: &Principal, cap: Capability) -> bool {
    principal.active
        && cap
            .granted_by()
            .iter()
            .any(|key| principal.has_org_role(key))
}
