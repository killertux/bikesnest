//! Parking versions, proposal diffs, and field-level pending-change cues.
use crate::{DetailsPage, i18n::Translator, view};
use bikesnest_application::ListingProposal;
use bikesnest_domain::{
    ParkingEdit, ProposalKind, ProposalStatus, ProposedChange, RevisionSummary, SecurityState,
};

pub struct ProfileValueVm {
    pub key: String,
    pub label: String,
    pub value: String,
}
pub struct ProfileDiffVm {
    pub key: String,
    pub label: String,
    pub current: String,
    pub proposed: String,
}
pub struct CollaborationProposalVm {
    pub id: i64,
    pub kind_label: String,
    pub reason: Option<String>,
    pub status: &'static str,
    pub approvals: i64,
    pub rejections: i64,
    pub changes: Vec<ProfileDiffVm>,
    pub stale: bool,
    pub manual_review: bool,
    pub can_vote: bool,
    pub created_label: String,
}
pub struct CollaborationRevisionVm {
    pub version: i64,
    pub created_label: String,
    pub values: Vec<ProfileValueVm>,
}

pub fn edit_values(edit: &ParkingEdit, tr: Translator) -> Vec<ProfileValueVm> {
    let mut fields = vec![
        ("name", "new.field.name", edit.name.clone()),
        ("address", "new.field.address", edit.address.clone()),
        (
            "description",
            "new.field.description",
            edit.description
                .clone()
                .unwrap_or_else(|| tr.t("proposal.value.unknown").into()),
        ),
        (
            "type",
            "new.field.type",
            view::type_label(tr, edit.parking_type).into(),
        ),
        ("cost", "new.field.cost", view::cost_label(tr, &edit.cost)),
        (
            "hours",
            "details.hours.title",
            view::hours_rows(tr, &edit.hours, chrono_tz::UTC, chrono::Utc::now())
                .iter()
                .map(|r| format!("{}: {}", r.day, r.label))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
    ]
    .into_iter()
    .map(|(key, label, value)| ProfileValueVm {
        key: key.into(),
        label: tr.t(label).into(),
        value,
    })
    .collect::<Vec<_>>();
    for code in bikesnest_domain::SECURITY_FEATURE_CODES {
        let state = edit
            .security
            .iter()
            .find(|f| f.code() == *code)
            .map(|f| f.state())
            .unwrap_or(SecurityState::Unknown);
        fields.push(ProfileValueVm {
            key: (*code).into(),
            label: tr.security(code).into(),
            value: tr
                .t(match state {
                    SecurityState::Yes => "details.security.yes",
                    SecurityState::No => "details.security.no",
                    SecurityState::Unknown => "details.security.unknown",
                })
                .into(),
        });
    }
    fields
}

pub fn snapshot_values(snapshot: &serde_json::Value, tr: Translator) -> Vec<ProfileValueVm> {
    let mut fields = ParkingEdit::from_json(snapshot)
        .map(|e| edit_values(&e, tr))
        .unwrap_or_default();
    // Older revisions may lack a full edit payload. Show only known public fields.
    if fields.is_empty() {
        for (key, label) in [
            ("name", "new.field.name"),
            ("address", "new.field.address"),
            ("description", "new.field.description"),
        ] {
            if let Some(value) = snapshot.get(key).and_then(|v| v.as_str()) {
                fields.push(ProfileValueVm {
                    key: key.into(),
                    label: tr.t(label).into(),
                    value: value.into(),
                });
            }
        }
    }
    if let (Some(lat), Some(lon)) = (
        snapshot["point"]["lat"].as_f64(),
        snapshot["point"]["lon"].as_f64(),
    ) {
        fields.push(ProfileValueVm {
            key: "point".into(),
            label: tr.t("proposal.field.coordinates").into(),
            value: format!("{lat:.6}, {lon:.6}"),
        });
    }
    if let Some(tz) = snapshot["timezone"].as_str() {
        fields.push(ProfileValueVm {
            key: "timezone".into(),
            label: tr.t("new.field.tz").into(),
            value: tz.into(),
        });
    }
    if let Some(state) = snapshot["moderation_state"].as_str() {
        fields.push(ProfileValueVm {
            key: "existence".into(),
            label: tr.t("proposal.field.existence").into(),
            value: tr
                .t(if state == "REMOVED" {
                    "proposal.existence.removed"
                } else {
                    "proposal.existence.exists"
                })
                .into(),
        });
    }
    fields
}

impl DetailsPage {
    pub fn collaboration_proposals(mut self, proposals: Vec<ListingProposal>) -> Self {
        self.collaboration_proposals = proposals
            .iter()
            .filter(|p| p.status == ProposalStatus::Pending)
            .map(|p| {
                let proposed = match &p.change {
                    ProposedChange::EditDetails(edit) => edit_values(edit, self.tr),
                    ProposedChange::MoveLocation { lat, lon, timezone } => {
                        let mut fields = vec![ProfileValueVm {
                            key: "point".into(),
                            label: self.tr.t("proposal.field.coordinates").into(),
                            value: format!("{lat:.6}, {lon:.6}"),
                        }];
                        if let Some(tz) = timezone {
                            fields.push(ProfileValueVm {
                                key: "timezone".into(),
                                label: self.tr.t("new.field.tz").into(),
                                value: tz.clone(),
                            });
                        }
                        fields
                    }
                    ProposedChange::ChangeExistence { exists } => vec![ProfileValueVm {
                        key: "existence".into(),
                        label: self.tr.t("proposal.field.existence").into(),
                        value: self
                            .tr
                            .t(if *exists {
                                "proposal.existence.exists"
                            } else {
                                "proposal.existence.removed"
                            })
                            .into(),
                    }],
                    ProposedChange::Unknown => Vec::new(),
                };
                let changes = proposed
                    .into_iter()
                    .filter_map(|f| {
                        let current = self
                            .published_values
                            .iter()
                            .find(|v| v.key == f.key)
                            .map(|v| v.value.clone())
                            .unwrap_or_else(|| self.tr.t("proposal.value.unknown").into());
                        (current != f.value).then_some(ProfileDiffVm {
                            key: f.key,
                            label: f.label,
                            current,
                            proposed: f.value,
                        })
                    })
                    .collect();
                let stale = p.base_version != self.version;
                CollaborationProposalVm {
                    id: p.id,
                    kind_label: self
                        .tr
                        .t(match p.kind {
                            ProposalKind::EditDetails => "profile.edit_details",
                            ProposalKind::MoveLocation => "proposal.kind.move",
                            ProposalKind::ChangeExistence => "proposal.kind.existence",
                        })
                        .into(),
                    reason: p.reason.clone(),
                    status: p.status.as_code(),
                    approvals: p.approvals,
                    rejections: p.rejections,
                    changes,
                    stale,
                    manual_review: p.change == ProposedChange::Unknown,
                    can_vote: self.can_contribute
                        && !stale
                        && p.proposer_id != self.viewer_id
                        && p.change != ProposedChange::Unknown,
                    created_label: view::iso_datetime_label(self.tr, p.created_at),
                }
            })
            .collect();
        // Older listings may not have a stored row for every security feature.
        // A proposed addition still needs a cue beside its unpublished state.
        for code in bikesnest_domain::SECURITY_FEATURE_CODES {
            if self.has_pending(code) && !self.security.iter().any(|f| f.code == *code) {
                self.security.push(crate::SecVm {
                    code: (*code).into(),
                    label: self.tr.security(code).into(),
                    state: "unknown",
                });
            }
        }
        self
    }
    pub fn collaboration_history(mut self, history: Vec<RevisionSummary>) -> Self {
        self.collaboration_history = history
            .into_iter()
            .map(|r| CollaborationRevisionVm {
                version: r.version,
                created_label: view::iso_datetime_label(self.tr, r.at),
                values: snapshot_values(&r.snapshot, self.tr),
            })
            .collect();
        self
    }
    pub fn has_pending(&self, key: &str) -> bool {
        self.collaboration_proposals
            .iter()
            .any(|p| !p.stale && p.changes.iter().any(|c| c.key == key))
    }
    pub fn pending_count(&self) -> i64 {
        self.collaboration_proposals.len() as i64 + self.pending_photos
    }
    pub fn pending_url(&self, key: &str) -> String {
        let anchor = self
            .collaboration_proposals
            .iter()
            .find(|p| !p.stale && p.changes.iter().any(|c| c.key == key))
            .map(|p| format!("#proposal-{}", p.id))
            .unwrap_or_default();
        format!("/parking/{}?tab=approvals{anchor}", self.id)
    }
}
