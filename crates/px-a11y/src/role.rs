//! The role model: WAI-ARIA 1.2 concrete roles plus the two roles the engine
//! itself produces ([`Role::Document`] for the root and [`Role::StaticText`]
//! for text runs).
//!
//! # What is and is not here
//!
//! Every concrete role in the ARIA 1.2 role list is a variant. The twelve
//! abstract roles (`command`, `composite`, `input`, `landmark`, `range`,
//! `roletype`, `section`, `sectionhead`, `select`, `structure`, `widget`,
//! `window`) are not variants at all: ARIA forbids authors from using them,
//! and having no variant means no code path can produce one. An explicit
//! `role` naming one is rejected ([`RoleError::Abstract`]).
//!
//! ARIA 1.3 draft roles (`mark`, `suggestion`, `comment`, `image`,
//! `sectionheader`, `sectionfooter`) are unknown tokens
//! here; they are rejected like any other unknown token, so the fallback list
//! moves on to the next token.

/// Where a role's accessible name may come from (ARIA "Name From").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameFrom {
    /// Only author-provided attributes and native labelling markup.
    Author,
    /// Author-provided names win, and subtree content is the fallback.
    Contents,
    /// The role cannot be named at all.
    Prohibited,
}

/// Why a role token was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoleError {
    /// The token names an abstract role.
    Abstract,
    /// The token names no role this engine knows.
    Unknown,
}

macro_rules! roles {
    ($( $(#[$m:meta])* $var:ident = $tok:literal ),* $(,)?) => {
        /// An accessibility role.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Role {
            $( $(#[$m])* $var, )*
        }

        impl Role {
            /// Every role, in declaration order.
            pub const ALL: &'static [Role] = &[ $( Role::$var, )* ];

            /// The role's ARIA token (`StaticText` has none in ARIA 1.2 and
            /// reports `text`).
            #[must_use]
            pub fn token(self) -> &'static str {
                match self { $( Role::$var => $tok, )* }
            }
        }
    };
}

roles! {
    Alert = "alert",
    AlertDialog = "alertdialog",
    Application = "application",
    Article = "article",
    Banner = "banner",
    Blockquote = "blockquote",
    Button = "button",
    Caption = "caption",
    Cell = "cell",
    Checkbox = "checkbox",
    Code = "code",
    ColumnHeader = "columnheader",
    Combobox = "combobox",
    Complementary = "complementary",
    ContentInfo = "contentinfo",
    Definition = "definition",
    Deletion = "deletion",
    Dialog = "dialog",
    Directory = "directory",
    /// The ARIA `document` role; also what the root node reports.
    Document = "document",
    Emphasis = "emphasis",
    Feed = "feed",
    Figure = "figure",
    Form = "form",
    Generic = "generic",
    Grid = "grid",
    GridCell = "gridcell",
    Group = "group",
    Heading = "heading",
    Img = "img",
    Insertion = "insertion",
    Link = "link",
    List = "list",
    Listbox = "listbox",
    ListItem = "listitem",
    Log = "log",
    Main = "main",
    Marquee = "marquee",
    Math = "math",
    Menu = "menu",
    MenuBar = "menubar",
    MenuItem = "menuitem",
    MenuItemCheckbox = "menuitemcheckbox",
    MenuItemRadio = "menuitemradio",
    Meter = "meter",
    Navigation = "navigation",
    Note = "note",
    Option = "option",
    Paragraph = "paragraph",
    /// `presentation` and its synonym `none`.
    Presentation = "presentation",
    ProgressBar = "progressbar",
    Radio = "radio",
    RadioGroup = "radiogroup",
    Region = "region",
    Row = "row",
    RowGroup = "rowgroup",
    RowHeader = "rowheader",
    ScrollBar = "scrollbar",
    Search = "search",
    SearchBox = "searchbox",
    Separator = "separator",
    Slider = "slider",
    SpinButton = "spinbutton",
    Status = "status",
    Strong = "strong",
    Subscript = "subscript",
    Superscript = "superscript",
    Switch = "switch",
    Tab = "tab",
    Table = "table",
    TabList = "tablist",
    TabPanel = "tabpanel",
    Term = "term",
    TextBox = "textbox",
    Time = "time",
    Timer = "timer",
    Toolbar = "toolbar",
    Tooltip = "tooltip",
    Tree = "tree",
    TreeGrid = "treegrid",
    TreeItem = "treeitem",
    /// A run of text. Engine-produced; never the result of a `role` attribute.
    StaticText = "text",
}

/// ARIA 1.2's abstract roles. Not variants of [`Role`]; listed so a token that
/// names one is reported as abstract rather than merely unknown.
pub const ABSTRACT_ROLES: [&str; 12] = [
    "command",
    "composite",
    "input",
    "landmark",
    "range",
    "roletype",
    "section",
    "sectionhead",
    "select",
    "structure",
    "widget",
    "window",
];

/// The longest role token considered. Real tokens are at most 16 bytes;
/// anything longer is unknown without being compared.
const MAX_TOKEN: usize = 32;

/// The most tokens of a fallback list considered.
const MAX_TOKENS: usize = 16;

/// ASCII whitespace as HTML defines it.
pub(crate) fn is_ascii_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\x0c' | '\r')
}

impl Role {
    /// Parse one role token, ASCII case-insensitively.
    ///
    /// # Errors
    ///
    /// [`RoleError::Abstract`] for an abstract role, [`RoleError::Unknown`]
    /// for anything else that is not a role.
    pub fn parse_token(token: &str) -> Result<Role, RoleError> {
        if token.len() > MAX_TOKEN {
            return Err(RoleError::Unknown);
        }
        if ABSTRACT_ROLES
            .iter()
            .any(|abstract_role| abstract_role.eq_ignore_ascii_case(token))
        {
            return Err(RoleError::Abstract);
        }
        if token.eq_ignore_ascii_case("none") {
            return Ok(Role::Presentation);
        }
        // `StaticText` is engine-only: "text" is not an ARIA 1.2 role.
        Role::ALL
            .iter()
            .copied()
            .find(|role| *role != Role::StaticText && role.token().eq_ignore_ascii_case(token))
            .ok_or(RoleError::Unknown)
    }

    /// The role a `role` attribute value selects: the first token of the
    /// space-separated fallback list that is a valid concrete role (ARIA 1.2
    /// section 5.4, "Roles", fallback roles). Abstract and unknown tokens are
    /// skipped. `None` when no token is usable, in which case the element's
    /// implicit role applies.
    #[must_use]
    pub fn parse_list(value: &str) -> Option<Role> {
        value
            .split(is_ascii_ws)
            .filter(|t| !t.is_empty())
            .take(MAX_TOKENS)
            .find_map(|t| Role::parse_token(t).ok())
    }

    /// Where this role's accessible name may come from.
    #[must_use]
    pub fn name_from(self) -> NameFrom {
        match self {
            Role::Button
            | Role::Cell
            | Role::Checkbox
            | Role::ColumnHeader
            | Role::GridCell
            | Role::Heading
            | Role::Link
            | Role::MenuItem
            | Role::MenuItemCheckbox
            | Role::MenuItemRadio
            | Role::Option
            | Role::Radio
            | Role::Row
            | Role::RowHeader
            | Role::Switch
            | Role::Tab
            | Role::Tooltip
            | Role::TreeItem
            | Role::StaticText => NameFrom::Contents,
            Role::Caption
            | Role::Code
            | Role::Deletion
            | Role::Emphasis
            | Role::Generic
            | Role::Insertion
            | Role::Paragraph
            | Role::Presentation
            | Role::Strong
            | Role::Subscript
            | Role::Superscript => NameFrom::Prohibited,
            _ => NameFrom::Author,
        }
    }

    /// Whether this role's children are presentational (ARIA "Children
    /// Presentational"): descendants are not exposed because the role's
    /// content is atomic to the user.
    #[must_use]
    pub fn children_presentational(self) -> bool {
        matches!(
            self,
            Role::Button
                | Role::Checkbox
                | Role::Img
                | Role::Math
                | Role::MenuItemCheckbox
                | Role::MenuItemRadio
                | Role::Meter
                | Role::Option
                | Role::ProgressBar
                | Role::Radio
                | Role::ScrollBar
                | Role::Separator
                | Role::Slider
                | Role::Switch
                | Role::Tab
        )
    }

    /// Roles whose accessible name is replaced, when embedded in another
    /// widget's label, by the control's value (accname "Embedded Control").
    pub(crate) fn is_embedded_control(self) -> bool {
        matches!(
            self,
            Role::TextBox
                | Role::SearchBox
                | Role::Combobox
                | Role::Listbox
                | Role::Slider
                | Role::SpinButton
                | Role::ScrollBar
        )
    }

    /// Roles that report a position in a set.
    pub(crate) fn has_set_position(self) -> bool {
        matches!(
            self,
            Role::ListItem
                | Role::Option
                | Role::Tab
                | Role::TreeItem
                | Role::MenuItem
                | Role::MenuItemCheckbox
                | Role::MenuItemRadio
                | Role::Radio
        )
    }

    /// Roles that report a heading-like level (`aria-level`).
    pub(crate) fn supports_level(self) -> bool {
        matches!(
            self,
            Role::Heading | Role::ListItem | Role::Row | Role::TreeItem
        )
    }
}
