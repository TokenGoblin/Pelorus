//! States and properties: plain data, `Copy` where it can be, `Eq` so the
//! diff can compare them.

/// A tri-state value (`aria-checked`, `aria-pressed`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tri {
    False,
    True,
    Mixed,
}

/// `aria-invalid`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Invalid {
    #[default]
    False,
    True,
    Grammar,
    Spelling,
}

/// `aria-current`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Current {
    #[default]
    False,
    True,
    Page,
    Step,
    Location,
    Date,
    Time,
}

/// `aria-haspopup`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HasPopup {
    #[default]
    False,
    Menu,
    Listbox,
    Tree,
    Grid,
    Dialog,
}

/// `aria-live`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Politeness {
    #[default]
    Off,
    Polite,
    Assertive,
}

/// The state flags of one node.
///
/// Optional fields are `None` when the state does not apply to the node (a
/// paragraph is not "unchecked"; it has no checked state at all). That
/// distinction is what a platform bridge needs to decide whether to expose the
/// state, so it is kept rather than flattened to `false`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct States {
    /// Can receive keyboard focus (natively, via `tabindex`, or
    /// `contenteditable`), and is not disabled or inert.
    pub focusable: bool,
    pub disabled: bool,
    pub required: bool,
    pub read_only: bool,
    pub invalid: Invalid,
    pub checked: Option<Tri>,
    pub pressed: Option<Tri>,
    pub selected: Option<bool>,
    pub expanded: Option<bool>,
    pub multiline: bool,
    pub modal: bool,
    pub busy: bool,
    pub current: Current,
    pub has_popup: HasPopup,
}

/// Which changes a live region announces (`aria-relevant`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Relevant {
    pub additions: bool,
    pub removals: bool,
    pub text: bool,
}

impl Default for Relevant {
    /// ARIA's default: `additions text`.
    fn default() -> Self {
        Self {
            additions: true,
            removals: false,
            text: true,
        }
    }
}

/// Live-region data (`aria-live`, `aria-atomic`, `aria-relevant`), including
/// what the role implies.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LiveRegion {
    pub politeness: Politeness,
    pub atomic: bool,
    pub relevant: Relevant,
}

/// A range widget's numbers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RangeValue {
    pub now: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

/// Non-boolean properties of a node.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Props {
    /// Heading level and similar (`aria-level`, or the `h1`-`h6` number).
    pub level: Option<u32>,
    /// 1-based position in the set, explicit or computed from siblings.
    pub pos_in_set: Option<u32>,
    /// Size of the set, explicit or computed.
    pub set_size: Option<u32>,
    pub live: LiveRegion,
    /// The current text value of a text entry control. Never set for a
    /// password field.
    pub value: Option<String>,
    pub range: Option<RangeValue>,
    /// Link target, for `a`/`area` with `href`.
    pub href: Option<String>,
}
