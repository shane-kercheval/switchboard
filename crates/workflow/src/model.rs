//! The parsed, validated in-memory model of a workflow file.
//!
//! These types are the output of [`crate::parse::parse_workflow`]; they mirror
//! the structure in `docs/workflow-spec.md` §"Top-level structure" / §"Steps".
//! They are built by hand-walking the YAML (rather than `serde` derive) so the
//! parser can emit precise, spec-worded errors and preserve input declaration
//! order — see the parser for the rationale.

/// A parsed workflow: the four top-level keys, with `inputs` kept in declaration
/// order (the invocation form renders fields in that order).
#[derive(Debug, Clone, PartialEq)]
pub struct Workflow {
    pub name: String,
    pub description: String,
    pub inputs: Vec<InputDecl>,
    pub steps: Vec<Step>,
}

/// One declared input. `optional` is derived at parse time: `true` for a `text?`
/// type or for any input carrying a `default` (per the spec, `default` implies
/// optional and `text?` is shorthand for an optional input defaulting to `""`).
#[derive(Debug, Clone, PartialEq)]
pub struct InputDecl {
    pub name: String,
    pub ty: InputType,
    pub description: Option<String>,
    pub default: Option<String>,
    pub optional: bool,
}

/// The input type grammar. The optional `text?` variant is *not* a distinct type
/// — it parses to [`InputType::Text`] with [`InputDecl::optional`] set, because
/// the `?` suffix is only valid on `text` in v1 (`agent?` / `prompt_id?` are
/// deferred to v2 and rejected at parse time).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputType {
    Agent,
    AgentList,
    PromptId,
    Text,
    TextList,
}

impl InputType {
    /// Whether this type carries agent reference(s) — used by invocation-time
    /// validation to decide which inputs to check against the project roster.
    #[must_use]
    pub fn is_agent(self) -> bool {
        matches!(self, InputType::Agent | InputType::AgentList)
    }

    /// Whether this type is a list (so a supplied value must be a YAML sequence).
    #[must_use]
    pub fn is_list(self) -> bool {
        matches!(self, InputType::AgentList | InputType::TextList)
    }
}

/// A value position that may be a single scalar (typically a `{{ template }}`)
/// **or** a hardcoded YAML list literal. The distinction is load-bearing: only
/// `List` literals are checked for emptiness/duplicates at parse time, because a
/// `Scalar` resolves to its list only at invocation (per the spec's parse-time
/// rules). Every contained string is a template (validated at parse time).
#[derive(Debug, Clone, PartialEq)]
pub enum Templated {
    Scalar(String),
    List(Vec<String>),
}

/// One workflow step. Externally tagged in the file (`{ send: {...} }`); the
/// parser enforces the "exactly one known step-type key" rule and rejects the
/// reserved v2 step keys (`if`, `branch`, `wait_for_first`).
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Send(SendStep),
    WaitFor(WaitForStep),
    WaitForAll(WaitForAllStep),
    PauseForUser(PauseForUserStep),
    ForEach(ForEachStep),
}

/// `send` (Primitive 2). At least one of `prompt` / `text` / `forward_from` is
/// present; `prompt` and `text` are mutually exclusive (enforced at parse time).
#[derive(Debug, Clone, PartialEq)]
pub struct SendStep {
    pub to: Templated,
    pub prompt: Option<String>,
    pub text: Option<String>,
    /// Declared-order `(name, templated_value)` pairs passed to the prompt
    /// template at render time. Kept ordered (not a map) for deterministic
    /// rendering and round-tripping.
    pub template_vars: Vec<(String, String)>,
    pub forward_from: Option<Templated>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WaitForStep {
    pub agent: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WaitForAllStep {
    pub agents: Templated,
}

/// `pause_for_user` (Primitive 5). `required` defaults to `true`.
#[derive(Debug, Clone, PartialEq)]
pub struct PauseForUserStep {
    pub context: Option<String>,
    pub recipient: Option<String>,
    pub required: bool,
}

/// `for_each` (Primitive 6). Nested `for_each` is rejected at parse time, so a
/// body never contains another `ForEach`.
#[derive(Debug, Clone, PartialEq)]
pub struct ForEachStep {
    pub item: String,
    pub r#in: Templated,
    pub steps: Vec<Step>,
}
