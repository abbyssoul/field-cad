//! Pure expression-editor draft state and candidate previews.
//!
//! This module deliberately has no egui dependency. Widgets are transient
//! views over these state machines; reconciliation remains unit-testable when
//! authoritative state changes because of MCP, undo/redo, or queue replay.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use fieldcad_core::WorldSnapshot;
use fieldcad_expressions::{
    EvaluationPlan, EvaluationResult, ExpressionDiagnostic, ExpressionDocument,
    ExpressionNodeState, ExpressionState, ExpressionSubject,
};

pub(super) struct WorldDistanceProvider<'a>(pub &'a WorldSnapshot);

impl fieldcad_expressions::ValueProvider for WorldDistanceProvider<'_> {
    fn distance(&self, probe: fieldcad_core::DistanceProbeId) -> Option<f64> {
        self.0
            .distance_probe(probe)
            .and_then(|probe| self.0.resolve_distance(probe).ok())
    }
}

/// Whether an edit has been handed to the authority but not acknowledged yet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum SubmissionState {
    #[default]
    Idle,
    Submitted,
}

/// Reconciliation shared by every expression-bearing editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AuthorityDraft<T> {
    baseline: T,
    graph_hash: String,
    edited: T,
    submission: SubmissionState,
    authority_changed: bool,
}

impl<T: Clone + PartialEq> AuthorityDraft<T> {
    pub(super) fn new(authoritative: T, graph_hash: impl Into<String>) -> Self {
        Self {
            baseline: authoritative.clone(),
            graph_hash: graph_hash.into(),
            edited: authoritative,
            submission: SubmissionState::Idle,
            authority_changed: false,
        }
    }

    pub(super) fn edited(&self) -> &T {
        &self.edited
    }

    pub(super) fn edited_mut(&mut self) -> &mut T {
        &mut self.edited
    }

    pub(super) fn dirty(&self) -> bool {
        self.edited != self.baseline
    }

    pub(super) const fn submission(&self) -> SubmissionState {
        self.submission
    }

    pub(super) const fn authority_changed(&self) -> bool {
        self.authority_changed
    }

    /// Incorporate a newly observed accepted value.
    pub(super) fn reconcile(&mut self, authoritative: T, graph_hash: impl Into<String>) {
        let graph_hash = graph_hash.into();
        if authoritative == self.edited || !self.dirty() {
            self.baseline = authoritative.clone();
            self.edited = authoritative;
            self.graph_hash = graph_hash;
            self.submission = SubmissionState::Idle;
            self.authority_changed = false;
        } else if authoritative != self.baseline || graph_hash != self.graph_hash {
            // Keep the local text, but make Reset/Cancel restore the newest
            // authority rather than the value that was current when typing began.
            self.baseline = authoritative;
            self.graph_hash = graph_hash;
            self.submission = SubmissionState::Idle;
            self.authority_changed = true;
        }
    }

    pub(super) fn mark_submitted(&mut self) {
        self.submission = SubmissionState::Submitted;
    }

    pub(super) fn reset(&mut self) {
        self.edited = self.baseline.clone();
        self.submission = SubmissionState::Idle;
        self.authority_changed = false;
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ConstantFields {
    pub name: String,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ExistingConstantDraft(pub AuthorityDraft<ConstantFields>);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct NewConstantDraft(pub AuthorityDraft<ConstantFields>);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PropertyFormulaDraft(pub AuthorityDraft<String>);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct UserLibraryDraft(pub AuthorityDraft<fieldcad_expressions::UserConstantLibrary>);

/// Candidate preview, kept separate from accepted live diagnostics.
pub(super) struct DraftPreview {
    pub values: Option<EvaluationResult>,
    pub diagnostic: Option<ExpressionDiagnostic>,
    pub dependents: Vec<ExpressionSubject>,
}

impl DraftPreview {
    pub(super) fn valid(&self) -> bool {
        self.diagnostic.is_none()
    }
}

pub(super) fn should_submit<T: Clone + PartialEq>(
    draft: &AuthorityDraft<T>,
    preview: &DraftPreview,
    requested: bool,
) -> bool {
    requested && draft.dirty() && preview.valid()
}

/// Display labels may change, but authored distance tokens depend only on the
/// stable probe identity.
pub(super) fn distance_insertions(world: &WorldSnapshot) -> Vec<(String, String)> {
    world
        .distance_probes()
        .iter()
        .map(|(id, probe)| {
            (
                format!("Distance: {}", probe.name),
                format!("distance.{}", id.get()),
            )
        })
        .collect()
}

/// Compile and evaluate a complete candidate graph. If compilation fails, the
/// accepted graph supplies the affected-target list because the candidate has
/// no trustworthy dependency topology.
pub(super) fn preview_document(
    document: &ExpressionDocument,
    world: &WorldSnapshot,
    drafted_subject: ExpressionSubject,
    accepted: &ExpressionState,
) -> DraftPreview {
    let mut plan = match EvaluationPlan::compile(document, |target| {
        let object = world.object(target.object)?;
        object.components.get(&target.component)?;
        let schema = world.component_schemas().get(&target.component)?;
        let property = schema
            .properties
            .iter()
            .find(|property| property.id == target.property)?;
        let fieldcad_core::PropertyKind::Scalar(dimension) = property.kind else {
            return None;
        };
        Some(fieldcad_expressions::PropertyBindingSchema {
            dimension,
            live_binding: property.live_binding,
        })
    }) {
        Ok(plan) => plan,
        Err(error) => {
            return DraftPreview {
                values: None,
                diagnostic: Some(ExpressionDiagnostic {
                    subject: drafted_subject.clone(),
                    error,
                }),
                dependents: transitive_dependents(&accepted.nodes, &drafted_subject),
            };
        }
    };

    match plan.evaluate_candidate(&WorldDistanceProvider(world)) {
        Ok(()) => {
            plan.adopt_candidate();
            let nodes = plan.node_states(&[]);
            DraftPreview {
                values: Some(EvaluationResult {
                    constants: plan.constant_values().collect(),
                    properties: plan
                        .property_values()
                        .map(|(target, value)| (target.clone(), value))
                        .collect(),
                }),
                diagnostic: None,
                dependents: transitive_dependents(&nodes, &drafted_subject),
            }
        }
        Err(diagnostic) => {
            let diagnostic = *diagnostic;
            let nodes = plan.node_states(std::slice::from_ref(&diagnostic));
            DraftPreview {
                values: None,
                dependents: transitive_dependents(&nodes, &drafted_subject),
                diagnostic: Some(diagnostic),
            }
        }
    }
}

fn transitive_dependents(
    nodes: &[ExpressionNodeState],
    subject: &ExpressionSubject,
) -> Vec<ExpressionSubject> {
    let mut reverse: BTreeMap<ExpressionSubject, Vec<ExpressionSubject>> = BTreeMap::new();
    for node in nodes {
        for dependency in &node.dependencies {
            if let fieldcad_expressions::ExpressionDependency::Constant(id) = dependency {
                reverse
                    .entry(ExpressionSubject::Constant(*id))
                    .or_default()
                    .push(node.subject.clone());
            }
        }
    }
    for dependents in reverse.values_mut() {
        dependents.sort();
        dependents.dedup();
    }
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([subject.clone()]);
    while let Some(current) = queue.pop_front() {
        for dependent in reverse.get(&current).into_iter().flatten() {
            if seen.insert(dependent.clone()) {
                queue.push_back(dependent.clone());
            }
        }
    }
    seen.into_iter().collect()
}

pub(super) fn subject_label(subject: &ExpressionSubject, document: &ExpressionDocument) -> String {
    match subject {
        ExpressionSubject::Constant(id) => document
            .constants
            .iter()
            .find(|constant| constant.id == *id)
            .map(|constant| format!("constant {}", constant.name))
            .unwrap_or_else(|| format!("constant {}", id.get())),
        ExpressionSubject::Property(target) => target.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fieldcad_core::{
        ComponentSchema, ComponentTypeId, Dimension, ObjectId, ObjectSpec, PluginId, PropertyBag,
        PropertyId, PropertyKind, PropertySchema, PropertyValue, Quantity, World, WorldCommand,
    };
    use fieldcad_expressions::{
        ConstantDefinition, ConstantId, ConstantScope, ExpressionCommand, ExpressionSource,
        PropertyBinding, PropertyTarget,
    };

    #[test]
    fn clean_draft_follows_authority() {
        let mut draft = AuthorityDraft::new("one".to_owned(), "a");
        draft.reconcile("two".to_owned(), "b");
        assert_eq!(draft.edited(), "two");
        assert!(!draft.dirty());
        assert!(!draft.authority_changed());
    }

    #[test]
    fn dirty_draft_survives_authority_and_reset_uses_latest_value() {
        let mut draft = AuthorityDraft::new("one".to_owned(), "a");
        *draft.edited_mut() = "local".to_owned();
        draft.mark_submitted();
        draft.reconcile("remote".to_owned(), "b");
        assert_eq!(draft.edited(), "local");
        assert!(draft.authority_changed());
        assert_eq!(draft.submission(), SubmissionState::Idle);
        draft.reset();
        assert_eq!(draft.edited(), "remote");
    }

    #[test]
    fn matching_accepted_update_acknowledges_submission() {
        let mut draft = AuthorityDraft::new("one".to_owned(), "a");
        *draft.edited_mut() = "local".to_owned();
        draft.mark_submitted();
        draft.reconcile("local".to_owned(), "b");
        assert!(!draft.dirty());
        assert_eq!(draft.submission(), SubmissionState::Idle);
        assert!(!draft.authority_changed());
    }

    fn preview_fixture() -> (WorldSnapshot, PropertyTarget) {
        let component = ComponentTypeId::new(PluginId::new("draft.test").unwrap(), "body").unwrap();
        let property = PropertyId::new("length").unwrap();
        let mut world = World::new();
        world
            .commit([
                WorldCommand::RegisterComponentSchema(ComponentSchema {
                    id: component.clone(),
                    display_name: "Body".to_owned(),
                    properties: vec![PropertySchema {
                        id: property.clone(),
                        display_name: "Length".to_owned(),
                        description: None,
                        kind: PropertyKind::Scalar(Dimension::LENGTH),
                        required: true,
                        live_binding: false,
                        default_value: None,
                        relevant_when: None,
                    }],
                }),
                WorldCommand::CreateObject(
                    ObjectSpec::new("body").with_component(
                        component.clone(),
                        [(
                            property.clone(),
                            PropertyValue::Scalar(Quantity::new(1.0, Dimension::LENGTH).unwrap()),
                        )]
                        .into_iter()
                        .collect::<PropertyBag>(),
                    ),
                ),
            ])
            .unwrap();
        (
            world.snapshot(),
            PropertyTarget {
                object: ObjectId::new(0),
                component,
                property,
            },
        )
    }

    #[test]
    fn preview_reports_owned_span_and_transitive_property_dependents() {
        let (world, target) = preview_fixture();
        let first = ConstantId::new(1);
        let second = ConstantId::new(2);
        let accepted_document = ExpressionDocument {
            constants: vec![
                ConstantDefinition {
                    id: first,
                    scope: ConstantScope::Document,
                    name: "first".to_owned(),
                    source: "1 m".into(),
                    revision: None,
                    provenance: None,
                    origin: None,
                },
                ConstantDefinition {
                    id: second,
                    scope: ConstantScope::Document,
                    name: "second".to_owned(),
                    source: "doc.first".into(),
                    revision: None,
                    provenance: None,
                    origin: None,
                },
            ],
            bindings: vec![PropertyBinding {
                target: target.clone(),
                source: "doc.second".into(),
            }],
        };
        let mut accepted_plan = EvaluationPlan::compile(&accepted_document, |_| {
            Some(fieldcad_expressions::PropertyBindingSchema {
                dimension: Dimension::LENGTH,
                live_binding: false,
            })
        })
        .unwrap();
        accepted_plan
            .evaluate(&WorldDistanceProvider(&world))
            .unwrap();
        let accepted = ExpressionState {
            document: accepted_document.clone(),
            graph_hash: accepted_document.content_hash(),
            resolved_world_revision: Some(world.revision()),
            nodes: accepted_plan.node_states(&[]),
            diagnostics: Vec::new(),
        };
        let candidate = accepted_document
            .apply([ExpressionCommand::SetConstantSource {
                constant: first,
                source: ExpressionSource::from("("),
            }])
            .unwrap();
        let preview = preview_document(
            &candidate,
            &world,
            ExpressionSubject::Constant(first),
            &accepted,
        );
        let diagnostic = preview.diagnostic.unwrap();
        assert_eq!(diagnostic.subject, ExpressionSubject::Constant(first));
        assert!(diagnostic.error.span.is_some());
        assert_eq!(
            preview.dependents,
            vec![
                ExpressionSubject::Constant(second),
                ExpressionSubject::Property(target)
            ]
        );
    }

    #[test]
    fn enter_submits_only_a_valid_dirty_draft() {
        let mut draft = AuthorityDraft::new("1 m".to_owned(), "accepted");
        *draft.edited_mut() = "2 m".to_owned();
        let valid = DraftPreview {
            values: None,
            diagnostic: None,
            dependents: Vec::new(),
        };
        assert!(should_submit(&draft, &valid, true));
        let invalid = DraftPreview {
            values: None,
            diagnostic: Some(ExpressionDiagnostic {
                subject: ExpressionSubject::Constant(ConstantId::new(1)),
                error: fieldcad_expressions::ExpressionError {
                    kind: fieldcad_expressions::ExpressionErrorKind::Cycle,
                    message: "cycle".to_owned(),
                    span: None,
                    dependents: Vec::new(),
                },
            }),
            dependents: Vec::new(),
        };
        assert!(!should_submit(&draft, &invalid, true));
        draft.reset();
        assert!(!should_submit(&draft, &valid, true));
    }

    #[test]
    fn distance_menu_label_follows_rename_but_token_stays_stable() {
        let mut world = World::new();
        let report = world
            .commit([
                WorldCommand::CreateObject(ObjectSpec::new("a")),
                WorldCommand::CreateObject(ObjectSpec::new("b")),
                WorldCommand::CreateDistanceProbe(fieldcad_core::DistanceProbeSpec::new(
                    "before",
                    ObjectId::new(0),
                    ObjectId::new(1),
                )),
            ])
            .unwrap();
        let probe = report.created_distance_probes[0];
        let before = distance_insertions(&world.snapshot());
        world
            .commit([WorldCommand::SetDistanceProbeName {
                probe,
                name: "after".to_owned(),
            }])
            .unwrap();
        let after = distance_insertions(&world.snapshot());
        assert_eq!(before[0].1, after[0].1);
        assert_eq!(after[0].0, "Distance: after");
    }
}
