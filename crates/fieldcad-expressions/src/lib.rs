//! Dimension-aware scalar expressions for authored Field CAD values.
//!
//! This crate deliberately knows nothing about solvers or UI. It compiles
//! authored source into a transient graph and evaluates ordinary finite SI
//! quantities for the authoritative world to adopt.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use fieldcad_core::{ComponentTypeId, Dimension, DistanceProbeId, ObjectId, PropertyId, Quantity};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Maximum UTF-8 bytes accepted for one expression.
pub const MAX_EXPRESSION_BYTES: usize = 4096;
/// Maximum AST nodes accepted for one expression.
pub const MAX_EXPRESSION_NODES: usize = 1024;
/// Maximum parenthesis/unary nesting accepted by the parser.
pub const MAX_EXPRESSION_DEPTH: usize = 64;

/// Authored expression text, retained exactly as entered.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExpressionSource(String);

impl ExpressionSource {
    /// Construct retained source. Resource limits are checked by compilation.
    pub fn new(source: impl Into<String>) -> Self {
        Self(source.into())
    }

    /// Read the authored text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ExpressionSource {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for ExpressionSource {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl fmt::Display for ExpressionSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Half-open UTF-8 byte range in authored source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSpan {
    /// First byte included in the diagnostic.
    pub start: usize,
    /// First byte after the diagnostic.
    pub end: usize,
}

impl SourceSpan {
    /// Construct a source span.
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }
}

/// Stable category for an expression diagnostic.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExpressionErrorKind {
    /// Source exceeded a declared resource bound.
    ResourceLimit,
    /// The token stream is malformed.
    Syntax,
    /// A unit name is not recognised.
    UnknownUnit,
    /// A scoped value name is not defined.
    UnknownSymbol,
    /// More than one definition has the same scoped name.
    AmbiguousSymbol,
    /// A reference violates constant scope direction.
    ScopeViolation,
    /// Addition, subtraction, or a target has incompatible dimensions.
    DimensionMismatch,
    /// A constant dependency cycle exists.
    Cycle,
    /// Evaluation divided by zero.
    DivisionByZero,
    /// Evaluation produced NaN or infinity.
    NonFinite,
    /// A referenced runtime observation is unavailable.
    MissingValue,
    /// A property or schema named by a binding is unavailable.
    MissingTarget,
    /// A live observation was used by a property that is static-only.
    LiveBindingNotSupported,
    /// A referenced definition or probe cannot be removed.
    ReferencedDefinition,
}

/// User-facing expression diagnostic with a stable source location.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct ExpressionError {
    /// Stable machine-readable category.
    pub kind: ExpressionErrorKind,
    /// Human-readable explanation.
    pub message: String,
    /// Relevant source range, when the error belongs to one expression.
    pub span: Option<SourceSpan>,
    /// Dependents involved in graph errors or deletion guards.
    #[serde(default)]
    pub dependents: Vec<String>,
}

impl ExpressionError {
    fn at(kind: ExpressionErrorKind, message: impl Into<String>, span: SourceSpan) -> Self {
        Self {
            kind,
            message: message.into(),
            span: Some(span),
            dependents: Vec::new(),
        }
    }

    fn graph(kind: ExpressionErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            span: None,
            dependents: Vec::new(),
        }
    }
}

/// A finite scalar value expressed in authoritative SI units.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExpressionValue {
    si_value: f64,
    dimension: Dimension,
}

impl ExpressionValue {
    /// Construct a finite value.
    pub fn new(si_value: f64, dimension: Dimension) -> Result<Self, ExpressionError> {
        if !si_value.is_finite() {
            return Err(ExpressionError::graph(
                ExpressionErrorKind::NonFinite,
                "expression value must be finite",
            ));
        }
        Ok(Self {
            si_value,
            dimension,
        })
    }

    /// SI magnitude.
    pub const fn si_value(self) -> f64 {
        self.si_value
    }

    /// Physical dimension.
    pub const fn dimension(self) -> Dimension {
        self.dimension
    }

    /// Convert to the core quantity accepted at schema boundaries.
    pub fn quantity(self) -> Quantity {
        Quantity::new(self.si_value, self.dimension).expect("ExpressionValue is finite")
    }
}

/// Durable identity for a constant; display names may change around it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConstantId(u64);

impl ConstantId {
    /// Construct a stable document-local identity.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Numeric identity for transport and persistence.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Namespace in which a constant is authored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ConstantScope {
    /// Constant authored by this experiment.
    Document,
    /// Reproducibly embedded copy of a user-library constant.
    User,
}

/// One authored document or embedded-library constant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConstantDefinition {
    /// Stable identity, unchanged by rename.
    pub id: ConstantId,
    /// Explicit namespace.
    pub scope: ConstantScope,
    /// Editable display/reference name within the namespace.
    pub name: String,
    /// Retained authored formula.
    pub source: ExpressionSource,
    /// Source revision for an embedded library definition.
    #[serde(default)]
    pub revision: Option<String>,
    /// Human-readable origin for embedded provenance.
    #[serde(default)]
    pub provenance: Option<String>,
}

/// Stable address of one scalar component property.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PropertyTarget {
    /// Object carrying the component.
    pub object: ObjectId,
    /// Attached component schema.
    pub component: ComponentTypeId,
    /// Scalar property within the component.
    pub property: PropertyId,
}

impl fmt::Display for PropertyTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "object {} / {} / {}",
            self.object, self.component, self.property
        )
    }
}

/// Authored formula driving a scalar property.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PropertyBinding {
    /// Stable property address.
    pub target: PropertyTarget,
    /// Retained authored formula.
    pub source: ExpressionSource,
}

/// Persisted authored expression state. Compiled nodes are never serialized.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpressionDocument {
    /// Document constants and embedded user-library dependencies.
    #[serde(default)]
    pub constants: Vec<ConstantDefinition>,
    /// Scalar property expressions.
    #[serde(default)]
    pub bindings: Vec<PropertyBinding>,
}

/// One authored mutation to an [`ExpressionDocument`]. An authority applies a
/// batch to a clone before compiling and validating the resulting world.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExpressionCommand {
    /// Add a new definition.
    AddConstant(ConstantDefinition),
    /// Replace a definition's formula.
    SetConstantSource {
        /// Stable definition identity.
        constant: ConstantId,
        /// New retained source.
        source: ExpressionSource,
    },
    /// Rename a definition and rewrite scoped references.
    RenameConstant {
        /// Stable definition identity.
        constant: ConstantId,
        /// Unique name in the existing scope.
        name: String,
    },
    /// Remove an unreferenced definition.
    RemoveConstant(ConstantId),
    /// Set or replace one property formula.
    SetPropertyExpression(PropertyBinding),
    /// Clear authored intent, leaving the world's current literal value frozen.
    ClearPropertyExpression(PropertyTarget),
    /// Import or explicitly refresh embedded user definitions supplied as data.
    ImportUserConstants(Vec<ConstantDefinition>),
}

/// Standalone, desktop-owned reusable constant library file.
///
/// Headless/runtime code receives selected definitions through
/// `ImportUserConstants`; it never reads this file or a desktop config path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserConstantLibrary {
    /// Stable file-format identity.
    pub format: String,
    /// Rejectable format version.
    pub format_version: u32,
    /// User-scoped definitions available for explicit embedding.
    #[serde(default)]
    pub constants: Vec<ConstantDefinition>,
}

impl Default for UserConstantLibrary {
    fn default() -> Self {
        Self {
            format: "fieldcad.user-constants/v1".to_owned(),
            format_version: 1,
            constants: Vec::new(),
        }
    }
}

impl UserConstantLibrary {
    /// Return the complete dependency closure needed to embed `name`.
    pub fn dependency_closure(
        &self,
        name: &str,
        provenance: &str,
    ) -> Result<Vec<ConstantDefinition>, ExpressionError> {
        if self.format != "fieldcad.user-constants/v1" || self.format_version > 1 {
            return Err(ExpressionError::graph(
                ExpressionErrorKind::Syntax,
                "unsupported user constant library format",
            ));
        }
        if self
            .constants
            .iter()
            .any(|item| item.scope != ConstantScope::User)
        {
            return Err(ExpressionError::graph(
                ExpressionErrorKind::ScopeViolation,
                "a user library may contain only user-scoped definitions",
            ));
        }
        let names = build_name_index(&self.constants)?;
        let root = names
            .get(&(ConstantScope::User, name.to_owned()))
            .copied()
            .ok_or_else(|| {
                ExpressionError::graph(
                    ExpressionErrorKind::UnknownSymbol,
                    format!("unknown user constant 'user.{name}'"),
                )
            })?;
        let by_id: BTreeMap<_, _> = self.constants.iter().map(|item| (item.id, item)).collect();
        let mut pending = vec![root];
        let mut selected = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if !selected.insert(id) {
                continue;
            }
            let definition = by_id[&id];
            pending.extend(scan_constant_dependencies(
                &definition.source,
                &names,
                ConstantScope::User,
            )?);
        }
        let mut closure: Vec<_> = self
            .constants
            .iter()
            .filter(|item| selected.contains(&item.id))
            .cloned()
            .collect();
        // Compile the selected closure to reject cycles before it is embedded.
        EvaluationPlan::compile(
            &ExpressionDocument {
                constants: closure.clone(),
                bindings: Vec::new(),
            },
            |_| None,
        )?;
        let revision = ExpressionDocument {
            constants: closure.clone(),
            bindings: Vec::new(),
        }
        .content_hash();
        for definition in &mut closure {
            definition.revision = Some(revision.clone());
            definition.provenance = Some(provenance.to_owned());
        }
        Ok(closure)
    }

    /// Stable identities whose local source differs from an embedded copy.
    pub fn available_updates(&self, document: &ExpressionDocument) -> Vec<ConstantId> {
        let local: BTreeMap<_, _> = self.constants.iter().map(|item| (item.id, item)).collect();
        document
            .constants
            .iter()
            .filter(|embedded| embedded.scope == ConstantScope::User)
            .filter_map(|embedded| {
                local
                    .get(&embedded.id)
                    .filter(|candidate| candidate.source != embedded.source)
                    .map(|_| embedded.id)
            })
            .collect()
    }
}

impl ExpressionDocument {
    /// Deterministic SHA-256 of authored graph content, independent of resolved values.
    pub fn content_hash(&self) -> String {
        let mut constants: Vec<_> = self.constants.iter().collect();
        constants.sort_by_key(|definition| (definition.scope as u8, definition.id));
        let mut bindings: Vec<_> = self.bindings.iter().collect();
        bindings.sort_by_key(|binding| &binding.target);
        let mut hash = Sha256::new();
        for definition in constants {
            hash.update([definition.scope as u8]);
            hash.update(definition.id.get().to_le_bytes());
            hash.update(definition.name.as_bytes());
            hash.update([0]);
            hash.update(definition.source.as_str().as_bytes());
            hash.update([0xff]);
        }
        for binding in bindings {
            hash.update(binding.target.object.get().to_le_bytes());
            hash.update(binding.target.component.to_string().as_bytes());
            hash.update([0]);
            hash.update(binding.target.property.as_str().as_bytes());
            hash.update([0]);
            hash.update(binding.source.as_str().as_bytes());
            hash.update([0xfe]);
        }
        format!("{:x}", hash.finalize())
    }

    /// Reject removing a constant while another definition or property uses it.
    pub fn remove_constant(
        &mut self,
        id: ConstantId,
    ) -> Result<ConstantDefinition, ExpressionError> {
        let Some(definition) = self.constants.iter().find(|item| item.id == id).cloned() else {
            return Err(ExpressionError::graph(
                ExpressionErrorKind::UnknownSymbol,
                format!("constant {} does not exist", id.get()),
            ));
        };
        let qualified = format!(
            "{}.{}",
            match definition.scope {
                ConstantScope::Document => "doc",
                ConstantScope::User => "user",
            },
            definition.name
        );
        let mut dependents = Vec::new();
        for item in &self.constants {
            if item.id != id && source_mentions_symbol(item.source.as_str(), &qualified) {
                dependents.push(format!("constant {}", item.name));
            }
        }
        for binding in &self.bindings {
            if source_mentions_symbol(binding.source.as_str(), &qualified) {
                dependents.push(binding.target.to_string());
            }
        }
        if !dependents.is_empty() {
            return Err(ExpressionError {
                kind: ExpressionErrorKind::ReferencedDefinition,
                message: format!("{qualified} is still referenced"),
                span: None,
                dependents,
            });
        }
        let index = self
            .constants
            .iter()
            .position(|item| item.id == id)
            .unwrap();
        Ok(self.constants.remove(index))
    }

    /// Apply a batch atomically to this authored value.
    pub fn apply(
        &self,
        commands: impl IntoIterator<Item = ExpressionCommand>,
    ) -> Result<Self, ExpressionError> {
        let mut candidate = self.clone();
        for command in commands {
            match command {
                ExpressionCommand::AddConstant(definition) => {
                    if candidate
                        .constants
                        .iter()
                        .any(|item| item.id == definition.id)
                    {
                        return Err(ExpressionError::graph(
                            ExpressionErrorKind::AmbiguousSymbol,
                            format!("constant identity {} already exists", definition.id.get()),
                        ));
                    }
                    candidate.constants.push(definition);
                }
                ExpressionCommand::SetConstantSource { constant, source } => {
                    candidate.constant_mut(constant)?.source = source;
                }
                ExpressionCommand::RenameConstant { constant, name } => {
                    validate_name(&name)?;
                    let definition = candidate.constant_mut(constant)?.clone();
                    if candidate.constants.iter().any(|item| {
                        item.id != constant && item.scope == definition.scope && item.name == name
                    }) {
                        return Err(ExpressionError::graph(
                            ExpressionErrorKind::AmbiguousSymbol,
                            format!("{}.{} already exists", scope_name(definition.scope), name),
                        ));
                    }
                    let old = format!("{}.{}", scope_name(definition.scope), definition.name);
                    let new = format!("{}.{}", scope_name(definition.scope), name);
                    for item in &mut candidate.constants {
                        item.source = rewrite_symbol(&item.source, &old, &new)?;
                    }
                    for binding in &mut candidate.bindings {
                        binding.source = rewrite_symbol(&binding.source, &old, &new)?;
                    }
                    candidate.constant_mut(constant)?.name = name;
                }
                ExpressionCommand::RemoveConstant(constant) => {
                    candidate.remove_constant(constant)?;
                }
                ExpressionCommand::SetPropertyExpression(binding) => {
                    candidate
                        .bindings
                        .retain(|item| item.target != binding.target);
                    candidate.bindings.push(binding);
                }
                ExpressionCommand::ClearPropertyExpression(target) => {
                    candidate.bindings.retain(|item| item.target != target);
                }
                ExpressionCommand::ImportUserConstants(definitions) => {
                    if definitions
                        .iter()
                        .any(|item| item.scope != ConstantScope::User)
                    {
                        return Err(ExpressionError::graph(
                            ExpressionErrorKind::ScopeViolation,
                            "only user-scoped constants may be imported",
                        ));
                    }
                    let imported: BTreeSet<_> = definitions.iter().map(|item| item.id).collect();
                    candidate.constants.retain(|item| {
                        item.scope != ConstantScope::User || !imported.contains(&item.id)
                    });
                    candidate.constants.extend(definitions);
                }
            }
        }
        build_name_index(&candidate.constants)?;
        Ok(candidate)
    }

    fn constant_mut(&mut self, id: ConstantId) -> Result<&mut ConstantDefinition, ExpressionError> {
        self.constants
            .iter_mut()
            .find(|item| item.id == id)
            .ok_or_else(|| {
                ExpressionError::graph(
                    ExpressionErrorKind::UnknownSymbol,
                    format!("constant {} does not exist", id.get()),
                )
            })
    }
}

fn rewrite_symbol(
    source: &ExpressionSource,
    old: &str,
    new: &str,
) -> Result<ExpressionSource, ExpressionError> {
    let tokens = Lexer::new(source.as_str()).tokenize()?;
    let mut rewritten = String::with_capacity(source.as_str().len() + new.len());
    let mut cursor = 0;
    for token in tokens {
        if token.kind == TokenKind::Ident(old.to_owned()) {
            rewritten.push_str(&source.as_str()[cursor..token.span.start]);
            rewritten.push_str(new);
            cursor = token.span.end;
        }
    }
    rewritten.push_str(&source.as_str()[cursor..]);
    Ok(ExpressionSource::new(rewritten))
}

fn source_mentions_symbol(source: &str, symbol: &str) -> bool {
    Lexer::new(source).tokenize().is_ok_and(|tokens| {
        tokens
            .iter()
            .any(|token| token.kind == TokenKind::Ident(symbol.into()))
    })
}

/// Runtime provider used by a compiled expression. Compilation never captures live values.
pub trait ValueProvider {
    /// Resolve a stable distance-probe reference in metres.
    fn distance(&self, probe: DistanceProbeId) -> Option<f64>;
}

/// Schema information needed to compile a property binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PropertyBindingSchema {
    /// Required final dimension.
    pub dimension: Dimension,
    /// Whether a distance observation may be read every tick.
    pub live_binding: bool,
}

#[derive(Clone, Debug, PartialEq)]
enum SymbolRef {
    Constant(ConstantId),
    Distance(DistanceProbeId),
}

#[derive(Clone, Debug, PartialEq)]
enum Expr {
    Literal {
        value: ExpressionValue,
        span: SourceSpan,
    },
    Symbol {
        reference: SymbolRef,
        dimension: Dimension,
        span: SourceSpan,
    },
    Unary {
        negative: bool,
        expression: Box<Expr>,
        span: SourceSpan,
    },
    Binary {
        operation: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
        dimension: Dimension,
        span: SourceSpan,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BinaryOp {
    Add,
    Subtract,
    Multiply,
    Divide,
}

impl Expr {
    fn dimension(&self) -> Dimension {
        match self {
            Self::Literal { value, .. } => value.dimension(),
            Self::Symbol { dimension, .. } | Self::Binary { dimension, .. } => *dimension,
            Self::Unary { expression, .. } => expression.dimension(),
        }
    }

    fn contains_distance(&self) -> bool {
        match self {
            Self::Symbol {
                reference: SymbolRef::Distance(_),
                ..
            } => true,
            Self::Unary { expression, .. } => expression.contains_distance(),
            Self::Binary { left, right, .. } => {
                left.contains_distance() || right.contains_distance()
            }
            _ => false,
        }
    }
}

/// Parsed and dimension-checked transient expression.
#[derive(Clone, Debug, PartialEq)]
pub struct CompiledExpression {
    source: ExpressionSource,
    root: Expr,
}

impl CompiledExpression {
    /// Retained source used to build this transient form.
    pub const fn source(&self) -> &ExpressionSource {
        &self.source
    }
    /// Statically inferred result dimension.
    pub fn dimension(&self) -> Dimension {
        self.root.dimension()
    }
    /// Whether this expression reads a live distance probe.
    pub fn is_live(&self) -> bool {
        self.root.contains_distance()
    }

    fn evaluate(
        &self,
        constants: &BTreeMap<ConstantId, ExpressionValue>,
        provider: &dyn ValueProvider,
    ) -> Result<ExpressionValue, ExpressionError> {
        evaluate_expr(&self.root, constants, provider)
    }
}

fn evaluate_expr(
    expression: &Expr,
    constants: &BTreeMap<ConstantId, ExpressionValue>,
    provider: &dyn ValueProvider,
) -> Result<ExpressionValue, ExpressionError> {
    match expression {
        Expr::Literal { value, .. } => Ok(*value),
        Expr::Symbol {
            reference,
            dimension,
            span,
        } => match reference {
            SymbolRef::Constant(id) => constants.get(id).copied().ok_or_else(|| {
                ExpressionError::at(
                    ExpressionErrorKind::MissingValue,
                    "constant has not been evaluated",
                    *span,
                )
            }),
            SymbolRef::Distance(id) => provider
                .distance(*id)
                .ok_or_else(|| {
                    ExpressionError::at(
                        ExpressionErrorKind::MissingValue,
                        format!("distance probe {id} is unavailable"),
                        *span,
                    )
                })
                .and_then(|value| ExpressionValue::new(value, *dimension)),
        },
        Expr::Unary {
            negative,
            expression,
            ..
        } => {
            let value = evaluate_expr(expression, constants, provider)?;
            ExpressionValue::new(
                if *negative {
                    -value.si_value()
                } else {
                    value.si_value()
                },
                value.dimension(),
            )
        }
        Expr::Binary {
            operation,
            left,
            right,
            dimension,
            span,
        } => {
            let left = evaluate_expr(left, constants, provider)?;
            let right = evaluate_expr(right, constants, provider)?;
            if *operation == BinaryOp::Divide && right.si_value() == 0.0 {
                return Err(ExpressionError::at(
                    ExpressionErrorKind::DivisionByZero,
                    "division by zero",
                    *span,
                ));
            }
            let value = match operation {
                BinaryOp::Add => left.si_value() + right.si_value(),
                BinaryOp::Subtract => left.si_value() - right.si_value(),
                BinaryOp::Multiply => left.si_value() * right.si_value(),
                BinaryOp::Divide => left.si_value() / right.si_value(),
            };
            ExpressionValue::new(value, *dimension).map_err(|mut error| {
                error.span = Some(*span);
                error
            })
        }
    }
}

#[derive(Clone, Debug)]
struct CompiledConstant {
    id: ConstantId,
    expression: CompiledExpression,
}

#[derive(Clone, Debug)]
struct CompiledBinding {
    target: PropertyTarget,
    expression: CompiledExpression,
}

/// Deterministically topologically sorted, allocation-reusing expression graph.
#[derive(Clone, Debug)]
pub struct EvaluationPlan {
    constants: Vec<CompiledConstant>,
    bindings: Vec<CompiledBinding>,
    values: BTreeMap<ConstantId, ExpressionValue>,
    hash: String,
}

/// Values resolved for one candidate world adoption or pre-tick update.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EvaluationResult {
    /// Resolved constants by stable identity.
    pub constants: BTreeMap<ConstantId, ExpressionValue>,
    /// Resolved scalar property values by stable target.
    pub properties: BTreeMap<PropertyTarget, ExpressionValue>,
}

impl EvaluationPlan {
    /// Compile a complete graph. `property_schema` is the authoritative schema lookup.
    pub fn compile(
        document: &ExpressionDocument,
        property_schema: impl Fn(&PropertyTarget) -> Option<PropertyBindingSchema>,
    ) -> Result<Self, ExpressionError> {
        let names = build_name_index(&document.constants)?;
        let by_id: BTreeMap<_, _> = document
            .constants
            .iter()
            .map(|item| (item.id, item))
            .collect();
        let mut states = BTreeMap::new();
        let mut compiled = BTreeMap::new();
        let mut order = Vec::new();
        let mut stack = Vec::new();
        for definition in &document.constants {
            compile_constant(
                definition.id,
                &by_id,
                &names,
                &mut states,
                &mut compiled,
                &mut order,
                &mut stack,
            )?;
        }
        let constants: Vec<CompiledConstant> = order
            .into_iter()
            .map(|id| CompiledConstant {
                id,
                expression: compiled.remove(&id).expect("topological id was compiled"),
            })
            .collect();

        let dimensions: BTreeMap<_, _> = constants
            .iter()
            .map(|item| (item.id, item.expression.dimension()))
            .collect();
        let environment = CompileEnvironment {
            names: &names,
            dimensions: &dimensions,
            current_scope: None,
        };
        let mut bindings = Vec::with_capacity(document.bindings.len());
        let mut targets = BTreeSet::new();
        for binding in &document.bindings {
            if !targets.insert(binding.target.clone()) {
                return Err(ExpressionError::graph(
                    ExpressionErrorKind::AmbiguousSymbol,
                    format!("property {} has more than one binding", binding.target),
                ));
            }
            let schema = property_schema(&binding.target).ok_or_else(|| {
                ExpressionError::graph(
                    ExpressionErrorKind::MissingTarget,
                    format!("property target {} is unavailable", binding.target),
                )
            })?;
            let expression = compile_source(&binding.source, &environment)?;
            if expression.dimension() != schema.dimension {
                return Err(ExpressionError::graph(
                    ExpressionErrorKind::DimensionMismatch,
                    format!(
                        "property {} requires {}, expression produces {}",
                        binding.target,
                        schema.dimension,
                        expression.dimension()
                    ),
                ));
            }
            if expression.is_live() && !schema.live_binding {
                return Err(ExpressionError::graph(
                    ExpressionErrorKind::LiveBindingNotSupported,
                    format!("property {} does not support live bindings", binding.target),
                ));
            }
            bindings.push(CompiledBinding {
                target: binding.target.clone(),
                expression,
            });
        }
        bindings.sort_by(|left, right| left.target.cmp(&right.target));
        Ok(Self {
            constants,
            bindings,
            values: BTreeMap::new(),
            hash: document.content_hash(),
        })
    }

    /// Authored graph content hash used in snapshot/run provenance.
    pub fn content_hash(&self) -> &str {
        &self.hash
    }

    /// Stable constant order used by evaluation.
    pub fn constant_order(&self) -> impl Iterator<Item = ConstantId> + '_ {
        self.constants.iter().map(|item| item.id)
    }

    /// Most recently evaluated constant values for inspection/provenance.
    pub fn constant_values(&self) -> impl Iterator<Item = (ConstantId, ExpressionValue)> + '_ {
        self.values.iter().map(|(id, value)| (*id, *value))
    }

    /// Evaluate in O(nodes + edges + referenced distances), reusing internal constant storage.
    pub fn evaluate(
        &mut self,
        provider: &dyn ValueProvider,
    ) -> Result<EvaluationResult, ExpressionError> {
        for constant in &self.constants {
            let value = constant.expression.evaluate(&self.values, provider)?;
            self.values.insert(constant.id, value);
        }
        let mut properties = BTreeMap::new();
        for binding in &self.bindings {
            properties.insert(
                binding.target.clone(),
                binding.expression.evaluate(&self.values, provider)?,
            );
        }
        Ok(EvaluationResult {
            constants: self.values.clone(),
            properties,
        })
    }

    /// Evaluate property outputs into caller-owned reusable storage.
    ///
    /// Once the graph shape is stable this overwrites existing map entries;
    /// it does not clear/reallocate constant or property nodes each tick.
    pub fn evaluate_properties_into(
        &mut self,
        provider: &dyn ValueProvider,
        properties: &mut BTreeMap<PropertyTarget, ExpressionValue>,
    ) -> Result<(), ExpressionError> {
        for constant in &self.constants {
            let value = constant.expression.evaluate(&self.values, provider)?;
            self.values.insert(constant.id, value);
        }
        for binding in &self.bindings {
            let value = binding.expression.evaluate(&self.values, provider)?;
            if let Some(existing) = properties.get_mut(&binding.target) {
                *existing = value;
            } else {
                properties.insert(binding.target.clone(), value);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VisitState {
    Visiting,
    Complete,
}

fn build_name_index(
    constants: &[ConstantDefinition],
) -> Result<BTreeMap<(ConstantScope, String), ConstantId>, ExpressionError> {
    let mut names = BTreeMap::new();
    let mut identities = BTreeSet::new();
    for definition in constants {
        if !identities.insert(definition.id) {
            return Err(ExpressionError::graph(
                ExpressionErrorKind::AmbiguousSymbol,
                format!(
                    "constant identity {} is used more than once",
                    definition.id.get()
                ),
            ));
        }
        validate_name(&definition.name)?;
        let key = (definition.scope, definition.name.clone());
        if names.insert(key, definition.id).is_some() {
            return Err(ExpressionError::graph(
                ExpressionErrorKind::AmbiguousSymbol,
                format!(
                    "{}.{} is defined more than once",
                    scope_name(definition.scope),
                    definition.name
                ),
            ));
        }
    }
    Ok(names)
}

fn validate_name(name: &str) -> Result<(), ExpressionError> {
    if name.is_empty()
        || !name.chars().enumerate().all(|(index, character)| {
            character.is_ascii_alphanumeric() || character == '_' && index > 0
        })
        || name.as_bytes()[0].is_ascii_digit()
    {
        return Err(ExpressionError::graph(
            ExpressionErrorKind::Syntax,
            format!("'{name}' is not a valid constant name"),
        ));
    }
    Ok(())
}

fn scope_name(scope: ConstantScope) -> &'static str {
    match scope {
        ConstantScope::Document => "doc",
        ConstantScope::User => "user",
    }
}

#[allow(clippy::too_many_arguments)]
fn compile_constant(
    id: ConstantId,
    definitions: &BTreeMap<ConstantId, &ConstantDefinition>,
    names: &BTreeMap<(ConstantScope, String), ConstantId>,
    states: &mut BTreeMap<ConstantId, VisitState>,
    compiled: &mut BTreeMap<ConstantId, CompiledExpression>,
    order: &mut Vec<ConstantId>,
    stack: &mut Vec<ConstantId>,
) -> Result<(), ExpressionError> {
    match states.get(&id) {
        Some(VisitState::Complete) => return Ok(()),
        Some(VisitState::Visiting) => {
            let start = stack.iter().position(|item| *item == id).unwrap_or(0);
            let cycle = stack[start..]
                .iter()
                .chain([&id])
                .map(|item| definitions[item].name.as_str())
                .collect::<Vec<_>>()
                .join(" -> ");
            return Err(ExpressionError::graph(
                ExpressionErrorKind::Cycle,
                format!("constant cycle: {cycle}"),
            ));
        }
        None => {}
    }
    states.insert(id, VisitState::Visiting);
    stack.push(id);
    let definition = definitions[&id];
    let dependencies = scan_constant_dependencies(&definition.source, names, definition.scope)?;
    for dependency in dependencies {
        compile_constant(
            dependency,
            definitions,
            names,
            states,
            compiled,
            order,
            stack,
        )?;
    }
    let dimensions: BTreeMap<_, _> = compiled
        .iter()
        .map(|(id, expression)| (*id, expression.dimension()))
        .collect();
    let environment = CompileEnvironment {
        names,
        dimensions: &dimensions,
        current_scope: Some(definition.scope),
    };
    let expression = compile_source(&definition.source, &environment)?;
    compiled.insert(id, expression);
    order.push(id);
    stack.pop();
    states.insert(id, VisitState::Complete);
    Ok(())
}

fn scan_constant_dependencies(
    source: &ExpressionSource,
    names: &BTreeMap<(ConstantScope, String), ConstantId>,
    current_scope: ConstantScope,
) -> Result<Vec<ConstantId>, ExpressionError> {
    let tokens = Lexer::new(source.as_str()).tokenize()?;
    let mut dependencies = BTreeSet::new();
    for token in tokens {
        if let TokenKind::Ident(name) = token.kind
            && let Some((scope, local)) = parse_constant_symbol(&name)
        {
            if current_scope == ConstantScope::User && scope == ConstantScope::Document {
                return Err(ExpressionError::at(
                    ExpressionErrorKind::ScopeViolation,
                    "user constants cannot reference document constants",
                    token.span,
                ));
            }
            let id = names.get(&(scope, local.to_owned())).ok_or_else(|| {
                ExpressionError::at(
                    ExpressionErrorKind::UnknownSymbol,
                    format!("unknown symbol '{name}'"),
                    token.span,
                )
            })?;
            dependencies.insert(*id);
        }
    }
    Ok(dependencies.into_iter().collect())
}

struct CompileEnvironment<'a> {
    names: &'a BTreeMap<(ConstantScope, String), ConstantId>,
    dimensions: &'a BTreeMap<ConstantId, Dimension>,
    current_scope: Option<ConstantScope>,
}

fn compile_source(
    source: &ExpressionSource,
    environment: &CompileEnvironment<'_>,
) -> Result<CompiledExpression, ExpressionError> {
    if source.as_str().len() > MAX_EXPRESSION_BYTES {
        return Err(ExpressionError::graph(
            ExpressionErrorKind::ResourceLimit,
            format!("expression exceeds {MAX_EXPRESSION_BYTES} bytes"),
        ));
    }
    let tokens = Lexer::new(source.as_str()).tokenize()?;
    let mut parser = Parser {
        tokens,
        cursor: 0,
        nodes: 0,
        depth: 0,
        environment,
    };
    let root = parser.parse_expression(0)?;
    let token = parser.peek();
    if token.kind != TokenKind::End {
        return Err(ExpressionError::at(
            ExpressionErrorKind::Syntax,
            "unexpected token",
            token.span,
        ));
    }
    Ok(CompiledExpression {
        source: source.clone(),
        root,
    })
}

fn parse_constant_symbol(name: &str) -> Option<(ConstantScope, &str)> {
    name.strip_prefix("doc.")
        .map(|name| (ConstantScope::Document, name))
        .or_else(|| {
            name.strip_prefix("user.")
                .map(|name| (ConstantScope::User, name))
        })
}

#[derive(Clone, Debug, PartialEq)]
enum TokenKind {
    Number(f64),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    LeftParen,
    RightParen,
    End,
}

#[derive(Clone, Debug, PartialEq)]
struct Token {
    kind: TokenKind,
    span: SourceSpan,
}

struct Lexer<'a> {
    source: &'a str,
    cursor: usize,
}

impl<'a> Lexer<'a> {
    const fn new(source: &'a str) -> Self {
        Self { source, cursor: 0 }
    }

    fn tokenize(mut self) -> Result<Vec<Token>, ExpressionError> {
        let mut tokens = Vec::new();
        while self.cursor < self.source.len() {
            let byte = self.source.as_bytes()[self.cursor];
            if byte.is_ascii_whitespace() {
                self.cursor += 1;
                continue;
            }
            let start = self.cursor;
            let kind = match byte {
                b'+' => {
                    self.cursor += 1;
                    TokenKind::Plus
                }
                b'-' => {
                    self.cursor += 1;
                    TokenKind::Minus
                }
                b'*' => {
                    self.cursor += 1;
                    TokenKind::Star
                }
                b'/' => {
                    self.cursor += 1;
                    TokenKind::Slash
                }
                b'^' => {
                    self.cursor += 1;
                    TokenKind::Caret
                }
                b'(' => {
                    self.cursor += 1;
                    TokenKind::LeftParen
                }
                b')' => {
                    self.cursor += 1;
                    TokenKind::RightParen
                }
                b'0'..=b'9' | b'.' => self.number()?,
                _ if (byte as char).is_ascii_alphabetic() || byte == b'_' || byte >= 0x80 => {
                    self.identifier()
                }
                _ => {
                    return Err(ExpressionError::at(
                        ExpressionErrorKind::Syntax,
                        format!(
                            "unsupported character '{}'",
                            self.source[start..].chars().next().unwrap()
                        ),
                        SourceSpan::new(start, start + 1),
                    ));
                }
            };
            tokens.push(Token {
                kind,
                span: SourceSpan::new(start, self.cursor),
            });
            if tokens.len() > MAX_EXPRESSION_NODES * 2 {
                return Err(ExpressionError::graph(
                    ExpressionErrorKind::ResourceLimit,
                    "expression has too many tokens",
                ));
            }
        }
        tokens.push(Token {
            kind: TokenKind::End,
            span: SourceSpan::new(self.source.len(), self.source.len()),
        });
        Ok(tokens)
    }

    fn number(&mut self) -> Result<TokenKind, ExpressionError> {
        let start = self.cursor;
        let bytes = self.source.as_bytes();
        let mut digits = false;
        while self.cursor < bytes.len() && bytes[self.cursor].is_ascii_digit() {
            digits = true;
            self.cursor += 1;
        }
        if self.cursor < bytes.len() && bytes[self.cursor] == b'.' {
            self.cursor += 1;
            while self.cursor < bytes.len() && bytes[self.cursor].is_ascii_digit() {
                digits = true;
                self.cursor += 1;
            }
        }
        if !digits {
            return Err(ExpressionError::at(
                ExpressionErrorKind::Syntax,
                "expected digits",
                SourceSpan::new(start, self.cursor),
            ));
        }
        if self.cursor < bytes.len() && matches!(bytes[self.cursor], b'e' | b'E') {
            self.cursor += 1;
            if self.cursor < bytes.len() && matches!(bytes[self.cursor], b'+' | b'-') {
                self.cursor += 1;
            }
            let exponent = self.cursor;
            while self.cursor < bytes.len() && bytes[self.cursor].is_ascii_digit() {
                self.cursor += 1;
            }
            if exponent == self.cursor {
                return Err(ExpressionError::at(
                    ExpressionErrorKind::Syntax,
                    "scientific notation requires an exponent",
                    SourceSpan::new(start, self.cursor),
                ));
            }
        }
        self.source[start..self.cursor]
            .parse()
            .map(TokenKind::Number)
            .map_err(|_| {
                ExpressionError::at(
                    ExpressionErrorKind::Syntax,
                    "invalid number",
                    SourceSpan::new(start, self.cursor),
                )
            })
    }

    fn identifier(&mut self) -> TokenKind {
        let start = self.cursor;
        for character in self.source[self.cursor..].chars() {
            if character.is_alphanumeric() || matches!(character, '_' | '.') || character == 'µ' {
                self.cursor += character.len_utf8();
            } else {
                break;
            }
        }
        TokenKind::Ident(self.source[start..self.cursor].to_owned())
    }
}

struct Parser<'a> {
    tokens: Vec<Token>,
    cursor: usize,
    nodes: usize,
    depth: usize,
    environment: &'a CompileEnvironment<'a>,
}

impl Parser<'_> {
    fn peek(&self) -> &Token {
        &self.tokens[self.cursor]
    }
    fn next(&mut self) -> Token {
        let token = self.tokens[self.cursor].clone();
        self.cursor += 1;
        token
    }
    fn node(&mut self, span: SourceSpan) -> Result<(), ExpressionError> {
        self.nodes += 1;
        if self.nodes > MAX_EXPRESSION_NODES {
            return Err(ExpressionError::at(
                ExpressionErrorKind::ResourceLimit,
                "expression has too many operations",
                span,
            ));
        }
        Ok(())
    }

    fn parse_expression(&mut self, minimum_precedence: u8) -> Result<Expr, ExpressionError> {
        let mut left = self.parse_prefix()?;
        loop {
            // Adjacent unit after a numeric/subexpression is implicit multiplication.
            let implicit =
                matches!(self.peek().kind, TokenKind::Ident(ref name) if unit(name).is_some());
            let (operation, precedence) = match self.peek().kind {
                TokenKind::Plus => (BinaryOp::Add, 1),
                TokenKind::Minus => (BinaryOp::Subtract, 1),
                TokenKind::Star => (BinaryOp::Multiply, 2),
                TokenKind::Slash => (BinaryOp::Divide, 2),
                _ if implicit => (BinaryOp::Multiply, 2),
                _ => break,
            };
            if precedence < minimum_precedence {
                break;
            }
            let start = expression_span(&left).start;
            if !implicit {
                self.next();
            }
            let right = self.parse_expression(precedence + 1)?;
            let span = SourceSpan::new(start, expression_span(&right).end);
            let dimension = match operation {
                BinaryOp::Add | BinaryOp::Subtract => {
                    if left.dimension() != right.dimension() {
                        return Err(ExpressionError::at(
                            ExpressionErrorKind::DimensionMismatch,
                            format!(
                                "cannot combine {} and {}",
                                left.dimension(),
                                right.dimension()
                            ),
                            span,
                        ));
                    }
                    left.dimension()
                }
                BinaryOp::Multiply => {
                    combine_dimensions(left.dimension(), right.dimension(), false, span)?
                }
                BinaryOp::Divide => {
                    combine_dimensions(left.dimension(), right.dimension(), true, span)?
                }
            };
            self.node(span)?;
            left = Expr::Binary {
                operation,
                left: Box::new(left),
                right: Box::new(right),
                dimension,
                span,
            };
        }
        Ok(left)
    }

    fn parse_prefix(&mut self) -> Result<Expr, ExpressionError> {
        let token = self.next();
        match token.kind {
            TokenKind::Plus | TokenKind::Minus => {
                self.enter(token.span)?;
                let expression = self.parse_prefix()?;
                self.depth -= 1;
                let span = SourceSpan::new(token.span.start, expression_span(&expression).end);
                self.node(span)?;
                Ok(Expr::Unary {
                    negative: token.kind == TokenKind::Minus,
                    expression: Box::new(expression),
                    span,
                })
            }
            TokenKind::Number(value) => {
                self.node(token.span)?;
                ExpressionValue::new(value, Dimension::DIMENSIONLESS).map(|value| Expr::Literal {
                    value,
                    span: token.span,
                })
            }
            TokenKind::Ident(name) => self.parse_identifier(name, token.span),
            TokenKind::LeftParen => {
                self.enter(token.span)?;
                let expression = self.parse_expression(0)?;
                self.depth -= 1;
                let closing = self.next();
                if closing.kind != TokenKind::RightParen {
                    return Err(ExpressionError::at(
                        ExpressionErrorKind::Syntax,
                        "expected ')'",
                        closing.span,
                    ));
                }
                Ok(expression)
            }
            TokenKind::End => Err(ExpressionError::at(
                ExpressionErrorKind::Syntax,
                "expected an expression",
                token.span,
            )),
            _ => Err(ExpressionError::at(
                ExpressionErrorKind::Syntax,
                "expected a number, symbol, unit, or '('",
                token.span,
            )),
        }
    }

    fn enter(&mut self, span: SourceSpan) -> Result<(), ExpressionError> {
        self.depth += 1;
        if self.depth > MAX_EXPRESSION_DEPTH {
            return Err(ExpressionError::at(
                ExpressionErrorKind::ResourceLimit,
                "expression nesting is too deep",
                span,
            ));
        }
        Ok(())
    }

    fn parse_identifier(
        &mut self,
        name: String,
        span: SourceSpan,
    ) -> Result<Expr, ExpressionError> {
        if let Some((scale, mut dimension)) = unit(&name) {
            if self.peek().kind == TokenKind::Caret {
                self.next();
                let sign = if self.peek().kind == TokenKind::Minus {
                    self.next();
                    -1
                } else {
                    1
                };
                let exponent_token = self.next();
                let TokenKind::Number(exponent) = exponent_token.kind else {
                    return Err(ExpressionError::at(
                        ExpressionErrorKind::Syntax,
                        "unit power must be an integer",
                        exponent_token.span,
                    ));
                };
                if exponent.fract() != 0.0 || exponent.abs() > i8::MAX as f64 {
                    return Err(ExpressionError::at(
                        ExpressionErrorKind::Syntax,
                        "unit power must fit an integer exponent",
                        exponent_token.span,
                    ));
                }
                let exponent = exponent as i8 * sign;
                dimension = scale_dimension(dimension, exponent, exponent_token.span)?;
                let end = exponent_token.span.end;
                let value = scale.powi(i32::from(exponent));
                self.node(SourceSpan::new(span.start, end))?;
                return ExpressionValue::new(value, dimension).map(|value| Expr::Literal {
                    value,
                    span: SourceSpan::new(span.start, end),
                });
            }
            self.node(span)?;
            return ExpressionValue::new(scale, dimension)
                .map(|value| Expr::Literal { value, span });
        }
        let (reference, dimension) = if let Some((scope, local)) = parse_constant_symbol(&name) {
            if self.environment.current_scope == Some(ConstantScope::User)
                && scope == ConstantScope::Document
            {
                return Err(ExpressionError::at(
                    ExpressionErrorKind::ScopeViolation,
                    "user constants cannot reference document constants",
                    span,
                ));
            }
            let id = self
                .environment
                .names
                .get(&(scope, local.to_owned()))
                .ok_or_else(|| {
                    ExpressionError::at(
                        ExpressionErrorKind::UnknownSymbol,
                        format!("unknown symbol '{name}'"),
                        span,
                    )
                })?;
            let dimension = *self.environment.dimensions.get(id).ok_or_else(|| {
                ExpressionError::at(
                    ExpressionErrorKind::Cycle,
                    format!("'{name}' is not available before its dependencies"),
                    span,
                )
            })?;
            (SymbolRef::Constant(*id), dimension)
        } else if let Some(raw) = name.strip_prefix("distance.") {
            let id = raw.parse::<u64>().map_err(|_| {
                ExpressionError::at(
                    ExpressionErrorKind::UnknownSymbol,
                    format!("invalid distance reference '{name}'"),
                    span,
                )
            })?;
            (
                SymbolRef::Distance(DistanceProbeId::new(id)),
                Dimension::LENGTH,
            )
        } else {
            return Err(ExpressionError::at(
                ExpressionErrorKind::UnknownSymbol,
                format!("unknown symbol '{name}'"),
                span,
            ));
        };
        self.node(span)?;
        Ok(Expr::Symbol {
            reference,
            dimension,
            span,
        })
    }
}

fn expression_span(expression: &Expr) -> SourceSpan {
    match expression {
        Expr::Literal { span, .. } => *span,
        Expr::Symbol { span, .. } | Expr::Unary { span, .. } | Expr::Binary { span, .. } => *span,
    }
}

fn combine_dimensions(
    left: Dimension,
    right: Dimension,
    divide: bool,
    span: SourceSpan,
) -> Result<Dimension, ExpressionError> {
    let sign: i16 = if divide { -1 } else { 1 };
    let combine = |a: i8, b: i8| {
        i8::try_from(i16::from(a) + sign * i16::from(b)).map_err(|_| {
            ExpressionError::at(
                ExpressionErrorKind::ResourceLimit,
                "dimension exponent overflow",
                span,
            )
        })
    };
    Ok(Dimension::new(
        combine(left.mass, right.mass)?,
        combine(left.length, right.length)?,
        combine(left.time, right.time)?,
        combine(left.current, right.current)?,
        combine(left.temperature, right.temperature)?,
        combine(left.amount, right.amount)?,
        combine(left.luminous_intensity, right.luminous_intensity)?,
    ))
}

fn scale_dimension(
    dimension: Dimension,
    exponent: i8,
    span: SourceSpan,
) -> Result<Dimension, ExpressionError> {
    let scale = |value: i8| {
        i8::try_from(i16::from(value) * i16::from(exponent)).map_err(|_| {
            ExpressionError::at(
                ExpressionErrorKind::ResourceLimit,
                "dimension exponent overflow",
                span,
            )
        })
    };
    Ok(Dimension::new(
        scale(dimension.mass)?,
        scale(dimension.length)?,
        scale(dimension.time)?,
        scale(dimension.current)?,
        scale(dimension.temperature)?,
        scale(dimension.amount)?,
        scale(dimension.luminous_intensity)?,
    ))
}

fn unit(name: &str) -> Option<(f64, Dimension)> {
    let named = match name {
        "m" => Some((1.0, Dimension::LENGTH)),
        "s" => Some((1.0, Dimension::TIME)),
        "kg" => Some((1.0, Dimension::MASS)),
        "g" => Some((1e-3, Dimension::MASS)),
        "A" => Some((1.0, Dimension::CURRENT)),
        "C" => Some((1.0, Dimension::CHARGE)),
        "V" => Some((1.0, Dimension::ELECTRIC_POTENTIAL)),
        "T" => Some((1.0, Dimension::MAGNETIC_FLUX_DENSITY)),
        "Hz" => Some((1.0, Dimension::new(0, 0, -1, 0, 0, 0, 0))),
        "N" => Some((1.0, Dimension::new(1, 1, -2, 0, 0, 0, 0))),
        "J" => Some((1.0, Dimension::new(1, 2, -2, 0, 0, 0, 0))),
        _ => None,
    };
    if named.is_some() {
        return named;
    }
    const PREFIXES: [(&str, f64); 17] = [
        ("da", 1e1),
        ("Y", 1e24),
        ("Z", 1e21),
        ("E", 1e18),
        ("P", 1e15),
        ("T", 1e12),
        ("G", 1e9),
        ("M", 1e6),
        ("k", 1e3),
        ("h", 1e2),
        ("d", 1e-1),
        ("c", 1e-2),
        ("m", 1e-3),
        ("µ", 1e-6),
        ("u", 1e-6),
        ("n", 1e-9),
        ("p", 1e-12),
    ];
    for (prefix, factor) in PREFIXES {
        if let Some(root) = name.strip_prefix(prefix) {
            let (root_scale, dimension) = match root {
                "m" => (1.0, Dimension::LENGTH),
                "s" => (1.0, Dimension::TIME),
                "g" => (1e-3, Dimension::MASS),
                "A" => (1.0, Dimension::CURRENT),
                "C" => (1.0, Dimension::CHARGE),
                "V" => (1.0, Dimension::ELECTRIC_POTENTIAL),
                "T" => (1.0, Dimension::MAGNETIC_FLUX_DENSITY),
                _ => continue,
            };
            return Some((factor * root_scale, dimension));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use fieldcad_core::{PluginId, PropertyId};

    struct Distances(BTreeMap<DistanceProbeId, f64>);
    impl ValueProvider for Distances {
        fn distance(&self, probe: DistanceProbeId) -> Option<f64> {
            self.0.get(&probe).copied()
        }
    }
    fn target() -> PropertyTarget {
        PropertyTarget {
            object: ObjectId::new(1),
            component: ComponentTypeId::new(PluginId::new("test").unwrap(), "shape").unwrap(),
            property: PropertyId::new("radius").unwrap(),
        }
    }
    fn compile_binding(source: &str, dimension: Dimension) -> EvaluationPlan {
        EvaluationPlan::compile(
            &ExpressionDocument {
                constants: vec![],
                bindings: vec![PropertyBinding {
                    target: target(),
                    source: source.into(),
                }],
            },
            |_| {
                Some(PropertyBindingSchema {
                    dimension,
                    live_binding: true,
                })
            },
        )
        .unwrap()
    }

    #[test]
    fn motivating_expression_uses_precedence_scientific_notation_and_prefixes() {
        let mut plan = compile_binding("(6400 / 2) * 1e3 km", Dimension::LENGTH);
        let result = plan.evaluate(&Distances(BTreeMap::new())).unwrap();
        assert_eq!(result.properties[&target()].si_value(), 3.2e9);
    }

    #[test]
    fn compound_units_and_integer_powers_are_dimension_checked() {
        let density = Dimension::new(1, -3, 0, 0, 0, 0, 0);
        let mut plan = compile_binding("2.7 g / cm^3", density);
        let result = plan.evaluate(&Distances(BTreeMap::new())).unwrap();
        assert!((result.properties[&target()].si_value() - 2700.0).abs() < 1e-10);
    }

    #[test]
    fn mismatched_target_is_rejected() {
        let error = EvaluationPlan::compile(
            &ExpressionDocument {
                constants: vec![],
                bindings: vec![PropertyBinding {
                    target: target(),
                    source: "1 kg".into(),
                }],
            },
            |_| {
                Some(PropertyBindingSchema {
                    dimension: Dimension::LENGTH,
                    live_binding: true,
                })
            },
        )
        .unwrap_err();
        assert_eq!(error.kind, ExpressionErrorKind::DimensionMismatch);

        let addition = EvaluationPlan::compile(
            &ExpressionDocument {
                constants: vec![],
                bindings: vec![PropertyBinding {
                    target: target(),
                    source: "1 m + 2 s".into(),
                }],
            },
            |_| {
                Some(PropertyBindingSchema {
                    dimension: Dimension::LENGTH,
                    live_binding: true,
                })
            },
        )
        .unwrap_err();
        assert_eq!(addition.span, Some(SourceSpan::new(0, 9)));
    }

    #[test]
    fn forward_references_are_sorted_and_cycles_rejected() {
        let constants = vec![
            ConstantDefinition {
                id: ConstantId::new(2),
                scope: ConstantScope::Document,
                name: "b".into(),
                source: "doc.a * 2".into(),
                revision: None,
                provenance: None,
            },
            ConstantDefinition {
                id: ConstantId::new(1),
                scope: ConstantScope::Document,
                name: "a".into(),
                source: "3 m".into(),
                revision: None,
                provenance: None,
            },
        ];
        let plan = EvaluationPlan::compile(
            &ExpressionDocument {
                constants,
                bindings: vec![],
            },
            |_| None,
        )
        .unwrap();
        assert_eq!(
            plan.constant_order().collect::<Vec<_>>(),
            vec![ConstantId::new(1), ConstantId::new(2)]
        );
        let cyclic = ExpressionDocument {
            constants: vec![
                ConstantDefinition {
                    id: ConstantId::new(1),
                    scope: ConstantScope::Document,
                    name: "a".into(),
                    source: "doc.b".into(),
                    revision: None,
                    provenance: None,
                },
                ConstantDefinition {
                    id: ConstantId::new(2),
                    scope: ConstantScope::Document,
                    name: "b".into(),
                    source: "doc.a".into(),
                    revision: None,
                    provenance: None,
                },
            ],
            bindings: vec![],
        };
        assert_eq!(
            EvaluationPlan::compile(&cyclic, |_| None).unwrap_err().kind,
            ExpressionErrorKind::Cycle
        );
    }

    #[test]
    fn user_scope_cannot_read_document_scope() {
        let document = ExpressionDocument {
            constants: vec![
                ConstantDefinition {
                    id: ConstantId::new(1),
                    scope: ConstantScope::Document,
                    name: "a".into(),
                    source: "1 m".into(),
                    revision: None,
                    provenance: None,
                },
                ConstantDefinition {
                    id: ConstantId::new(2),
                    scope: ConstantScope::User,
                    name: "b".into(),
                    source: "doc.a".into(),
                    revision: None,
                    provenance: None,
                },
            ],
            bindings: vec![],
        };
        assert_eq!(
            EvaluationPlan::compile(&document, |_| None)
                .unwrap_err()
                .kind,
            ExpressionErrorKind::ScopeViolation
        );
    }

    #[test]
    fn live_distance_uses_stable_id_and_static_schema_rejects_it() {
        let document = ExpressionDocument {
            constants: vec![],
            bindings: vec![PropertyBinding {
                target: target(),
                source: "distance.7 / 2".into(),
            }],
        };
        let error = EvaluationPlan::compile(&document, |_| {
            Some(PropertyBindingSchema {
                dimension: Dimension::LENGTH,
                live_binding: false,
            })
        })
        .unwrap_err();
        assert_eq!(error.kind, ExpressionErrorKind::LiveBindingNotSupported);
        let mut plan = EvaluationPlan::compile(&document, |_| {
            Some(PropertyBindingSchema {
                dimension: Dimension::LENGTH,
                live_binding: true,
            })
        })
        .unwrap();
        let result = plan
            .evaluate(&Distances([(DistanceProbeId::new(7), 10.0)].into()))
            .unwrap();
        assert_eq!(result.properties[&target()].si_value(), 5.0);
    }

    #[test]
    fn division_by_zero_and_non_finite_results_are_rejected() {
        let mut zero = compile_binding("1 m / 0", Dimension::LENGTH);
        assert_eq!(
            zero.evaluate(&Distances(BTreeMap::new())).unwrap_err().kind,
            ExpressionErrorKind::DivisionByZero
        );
        let mut overflow = compile_binding("1e308 m * 1e308", Dimension::LENGTH);
        assert_eq!(
            overflow
                .evaluate(&Distances(BTreeMap::new()))
                .unwrap_err()
                .kind,
            ExpressionErrorKind::NonFinite
        );
    }

    #[test]
    fn malformed_input_reports_byte_span() {
        let error = EvaluationPlan::compile(
            &ExpressionDocument {
                constants: vec![],
                bindings: vec![PropertyBinding {
                    target: target(),
                    source: "1e+ m".into(),
                }],
            },
            |_| {
                Some(PropertyBindingSchema {
                    dimension: Dimension::LENGTH,
                    live_binding: true,
                })
            },
        )
        .unwrap_err();
        assert_eq!(error.kind, ExpressionErrorKind::Syntax);
        assert_eq!(error.span, Some(SourceSpan::new(0, 3)));
    }

    #[test]
    fn renaming_preserves_identity_and_rewrites_only_symbol_tokens() {
        let document = ExpressionDocument {
            constants: vec![ConstantDefinition {
                id: ConstantId::new(1),
                scope: ConstantScope::Document,
                name: "radius".into(),
                source: "2 m".into(),
                revision: None,
                provenance: None,
            }],
            bindings: vec![PropertyBinding {
                target: target(),
                source: "doc.radius + 1 m".into(),
            }],
        };
        let renamed = document
            .apply([ExpressionCommand::RenameConstant {
                constant: ConstantId::new(1),
                name: "size".into(),
            }])
            .unwrap();
        assert_eq!(renamed.constants[0].id, ConstantId::new(1));
        assert_eq!(renamed.bindings[0].source.as_str(), "doc.size + 1 m");
        let error = renamed
            .clone()
            .remove_constant(ConstantId::new(1))
            .unwrap_err();
        assert_eq!(error.kind, ExpressionErrorKind::ReferencedDefinition);
        assert_eq!(error.dependents, vec![target().to_string()]);
    }

    #[test]
    fn user_library_embeds_a_reproducible_dependency_closure() {
        let library = UserConstantLibrary {
            constants: vec![
                ConstantDefinition {
                    id: ConstantId::new(1),
                    scope: ConstantScope::User,
                    name: "density".into(),
                    source: "2.7 g / cm^3".into(),
                    revision: None,
                    provenance: None,
                },
                ConstantDefinition {
                    id: ConstantId::new(2),
                    scope: ConstantScope::User,
                    name: "double_density".into(),
                    source: "user.density * 2".into(),
                    revision: None,
                    provenance: None,
                },
                ConstantDefinition {
                    id: ConstantId::new(3),
                    scope: ConstantScope::User,
                    name: "unrelated".into(),
                    source: "1 s".into(),
                    revision: None,
                    provenance: None,
                },
            ],
            ..UserConstantLibrary::default()
        };
        let closure = library
            .dependency_closure("double_density", "user-constants.json")
            .unwrap();
        assert_eq!(
            closure.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![ConstantId::new(1), ConstantId::new(2)]
        );
        assert!(closure.iter().all(|item| item.revision.is_some()));
        assert!(
            closure
                .iter()
                .all(|item| item.provenance.as_deref() == Some("user-constants.json"))
        );
    }
}
