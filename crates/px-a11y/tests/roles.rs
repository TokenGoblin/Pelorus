//! Role mapping: HTML-AAM implicit roles, ARIA 1.2 explicit roles, the
//! fallback-list rule, abstract and invalid roles.
//!
//! Expectations are read off the specifications (HTML-AAM "Element Role
//! Mappings"; ARIA 1.2 section 5.4 on fallback roles and abstract roles), not
//! off the implementation. Every case marks the element under test `id="x"`.
//! `-` means the element is not exposed in the tree (generic or presentational
//! elements are collapsed away and their children promoted).

mod common;

use common::{Page, role_of};
use px_a11y::{ABSTRACT_ROLES, NameFrom, Role, RoleError};

fn check(cases: &[(&str, &str)]) {
    for (html, expected) in cases {
        assert_eq!(role_of(html), *expected, "for {html}");
    }
}

#[test]
fn a11y_role_links_and_buttons() {
    check(&[
        ("<a id=x href=u>t</a>", "link"),
        ("<a id=x>t</a>", "-"),
        ("<map><area id=x href=u alt=a></map>", "-"), // `area` is never rendered by default
        ("<button id=x>b</button>", "button"),
        ("<details><summary id=x>s</summary>c</details>", "button"),
    ]);
}

#[test]
fn a11y_role_headings_carry_their_level() {
    for level in 1..=6u32 {
        let p = Page::new(&format!("<h{level} id=x>t</h{level}>"));
        let n = p.node("x").expect("heading is exposed");
        assert_eq!(n.role(), Role::Heading);
        assert_eq!(n.props().level, Some(level));
    }
}

#[test]
fn a11y_role_lists_and_tables() {
    check(&[
        ("<ul id=x><li>a</li></ul>", "list"),
        ("<ol id=x><li>a</li></ol>", "list"),
        ("<menu id=x><li>a</li></menu>", "list"),
        ("<ul><li id=x>a</li></ul>", "listitem"),
        ("<dl><dt id=x>t</dt><dd>d</dd></dl>", "term"),
        ("<dl><dt>t</dt><dd id=x>d</dd></dl>", "definition"),
        ("<table id=x><tr><td>c</td></tr></table>", "table"),
        ("<table><tr id=x><td>c</td></tr></table>", "row"),
        ("<table><tr><td id=x>c</td></tr></table>", "cell"),
        (
            "<table><thead id=x><tr><th>h</th></tr></thead></table>",
            "rowgroup",
        ),
        (
            "<table><tbody id=x><tr><td>c</td></tr></tbody></table>",
            "rowgroup",
        ),
        (
            "<table><thead><tr><th id=x>h</th></tr></thead></table>",
            "columnheader",
        ),
        (
            "<table><tr><th id=x scope=row>h</th><td>c</td></tr></table>",
            "rowheader",
        ),
        (
            "<table><tr><th id=x scope=col>h</th></tr></table>",
            "columnheader",
        ),
        (
            "<table><caption id=x>c</caption><tr><td>c</td></tr></table>",
            "caption",
        ),
        (
            "<table role=grid><tr><td id=x>c</td></tr></table>",
            "gridcell",
        ),
    ]);
}

#[test]
fn a11y_role_landmarks_follow_their_scoping_rules() {
    check(&[
        ("<nav id=x>n</nav>", "navigation"),
        ("<main id=x>m</main>", "main"),
        ("<search id=x>s</search>", "search"),
        // header and footer are banner / contentinfo only when scoped to body.
        ("<header id=x>h</header>", "banner"),
        ("<footer id=x>f</footer>", "contentinfo"),
        ("<article><header id=x>h</header></article>", "-"),
        (
            "<section aria-label=s><footer id=x>f</footer></section>",
            "-",
        ),
        ("<main><header id=x>h</header></main>", "-"),
        ("<nav><footer id=x>f</footer></nav>", "-"),
        ("<aside><aside id=x>a</aside></aside>", "-"),
        // aside is complementary at body or main level, and in sectioning
        // content only when it has a name.
        ("<aside id=x>a</aside>", "complementary"),
        ("<main><aside id=x>a</aside></main>", "complementary"),
        ("<article><aside id=x>a</aside></article>", "-"),
        (
            "<article><aside id=x aria-label=n>a</aside></article>",
            "complementary",
        ),
        // section is region only with an accessible name.
        ("<section id=x>s</section>", "-"),
        ("<section id=x aria-label=n>s</section>", "region"),
        (
            "<section id=x aria-labelledby=h><h2 id=h>Head</h2></section>",
            "region",
        ),
        ("<section id=x title=t>s</section>", "region"),
        ("<form id=x>f</form>", "-"),
        ("<form id=x aria-label=n>f</form>", "form"),
        ("<article id=x>a</article>", "article"),
    ]);
}

#[test]
fn a11y_role_images() {
    check(&[
        ("<img id=x alt=a>", "img"),
        ("<img id=x>", "img"),
        ("<img id=x alt=''>", "-"),
        ("<img id=x alt='  '>", "-"),
        // Something else makes a decorative image meaningful.
        ("<img id=x alt='' title=t>", "img"),
        ("<img id=x alt='' aria-label=l>", "img"),
        ("<img id=x alt='' tabindex=0>", "img"),
        ("<math id=x></math>", "math"),
    ]);
}

#[test]
fn a11y_role_form_controls_by_input_type() {
    let cases = [
        ("button", "button"),
        ("submit", "button"),
        ("reset", "button"),
        ("image", "button"),
        ("checkbox", "checkbox"),
        ("radio", "radio"),
        ("range", "slider"),
        ("number", "spinbutton"),
        ("search", "searchbox"),
        ("text", "textbox"),
        ("email", "textbox"),
        ("tel", "textbox"),
        ("url", "textbox"),
        ("password", "textbox"),
        ("TEXT", "textbox"),
        ("", "textbox"),
        ("nonsense", "textbox"),
        // HTML-AAM: no corresponding role. Focusable, so kept as generic.
        ("color", "generic"),
        ("date", "generic"),
        ("file", "generic"),
        // hidden is not rendered.
        ("hidden", "-"),
    ];
    for (ty, role) in cases {
        let html = format!("<input id=x type='{ty}'>");
        assert_eq!(role_of(&html), role, "type={ty}");
    }
    check(&[
        (
            "<input id=x type=text list=l><datalist id=l></datalist>",
            "combobox",
        ),
        (
            "<input id=x type=search list=l><datalist id=l></datalist>",
            "combobox",
        ),
        ("<textarea id=x></textarea>", "textbox"),
        ("<select id=x><option>a</option></select>", "combobox"),
        (
            "<select id=x multiple><option>a</option></select>",
            "listbox",
        ),
        ("<select id=x size=4><option>a</option></select>", "listbox"),
        (
            "<select id=x size=1><option>a</option></select>",
            "combobox",
        ),
        ("<select><option id=x>a</option></select>", "option"),
        (
            "<select><optgroup id=x label=g><option>a</option></optgroup></select>",
            "group",
        ),
        ("<progress id=x value=1></progress>", "progressbar"),
        ("<meter id=x value=1></meter>", "meter"),
        ("<output id=x>o</output>", "status"),
        ("<fieldset id=x><legend>l</legend></fieldset>", "group"),
    ]);
}

#[test]
fn a11y_role_text_and_grouping_elements() {
    check(&[
        ("<p id=x>t</p>", "paragraph"),
        ("<blockquote id=x>t</blockquote>", "blockquote"),
        ("<figure id=x>t</figure>", "figure"),
        (
            "<figure><figcaption id=x>t</figcaption></figure>",
            "caption",
        ),
        ("<hr id=x>", "separator"),
        ("<dialog id=x open>d</dialog>", "dialog"),
        ("<dialog id=x>d</dialog>", "-"),
        ("<details id=x>d</details>", "group"),
        ("<address id=x>a</address>", "group"),
        ("<strong id=x>s</strong>", "strong"),
        ("<em id=x>s</em>", "emphasis"),
        ("<code id=x>s</code>", "code"),
        ("<del id=x>s</del>", "deletion"),
        ("<s id=x>s</s>", "deletion"),
        ("<ins id=x>s</ins>", "insertion"),
        ("<sub id=x>s</sub>", "subscript"),
        ("<sup id=x>s</sup>", "superscript"),
        ("<time id=x>s</time>", "time"),
        ("<dfn id=x>s</dfn>", "term"),
        // Elements with no role collapse.
        ("<div id=x>d</div>", "-"),
        ("<span id=x>d</span>", "-"),
        ("<pre id=x>d</pre>", "-"),
        ("<label id=x>d</label>", "-"),
        ("<iframe id=x></iframe>", "generic"), // focusable, so kept
    ]);
}

#[test]
fn a11y_role_explicit_role_overrides_and_fallback_list() {
    check(&[
        ("<div id=x role=button>b</div>", "button"),
        ("<div id=x role=BUTTON>b</div>", "button"),
        // First valid token wins.
        ("<div id=x role='navigation main'>b</div>", "navigation"),
        ("<div id=x role='  \t\n main  '>b</div>", "main"),
        // Unknown and abstract tokens are skipped.
        ("<div id=x role='foo button'>b</div>", "button"),
        ("<div id=x role='widget button'>b</div>", "button"),
        ("<div id=x role='landmark navigation'>b</div>", "navigation"),
        // Nothing valid: the implicit role applies.
        ("<div id=x role=widget>b</div>", "-"),
        ("<div id=x role='widget foo'>b</div>", "-"),
        ("<button id=x role=widget>b</button>", "button"),
        ("<a id=x href=u role=''>b</a>", "link"),
        // Explicit overrides implicit.
        ("<a id=x href=u role=button>b</a>", "button"),
        ("<h1 id=x role=tab>b</h1>", "tab"),
        ("<ul id=x role=menu><li>a</li></ul>", "menu"),
        // ARIA 1.3 tokens are not ARIA 1.2 roles.
        ("<div id=x role=mark>b</div>", "-"),
        ("<div id=x role=sectionheader>b</div>", "-"),
        ("<div id=x role=image>b</div>", "-"),
        // `none` and `presentation` collapse the element.
        ("<div id=x role=none>b</div>", "-"),
        ("<ul id=x role=presentation><li>a</li></ul>", "-"),
    ]);
}

#[test]
fn a11y_role_presentational_conflict_resolution() {
    // ARIA: a presentational role is ignored if the element is focusable or
    // carries global ARIA attributes.
    check(&[
        ("<button id=x role=presentation>b</button>", "button"),
        ("<div id=x role=none tabindex=0>b</div>", "generic"),
        ("<h1 id=x role=none aria-label=l>b</h1>", "heading"),
        ("<h1 id=x role=none>b</h1>", "-"),
    ]);
}

#[test]
fn a11y_role_presentational_table_and_list_children_are_presentational() {
    let p = Page::new(
        "<table role=presentation id=t><tr id=r><td id=c>cell</td></tr></table>\
         <ul role=none><li id=i>item</li></ul>",
    );
    for id in ["t", "r", "c", "i"] {
        assert_eq!(p.role(id), "-", "{id}");
    }
    // The text survives, promoted.
    assert_eq!(p.lines(), "text \"cell\"\ntext \"item\"\n");
}

#[test]
fn a11y_role_token_parsing() {
    assert_eq!(Role::parse_token("button"), Ok(Role::Button));
    assert_eq!(Role::parse_token("BuTtOn"), Ok(Role::Button));
    assert_eq!(Role::parse_token("none"), Ok(Role::Presentation));
    assert_eq!(Role::parse_token("presentation"), Ok(Role::Presentation));
    assert_eq!(Role::parse_token("nope"), Err(RoleError::Unknown));
    assert_eq!(Role::parse_token(""), Err(RoleError::Unknown));
    // The engine-only text role is not an ARIA 1.2 role.
    assert_eq!(Role::parse_token("text"), Err(RoleError::Unknown));
    // A token that is too long is unknown, not a hang.
    assert_eq!(
        Role::parse_token(&"a".repeat(1 << 20)),
        Err(RoleError::Unknown)
    );
}

#[test]
fn a11y_role_every_abstract_role_is_rejected() {
    assert_eq!(ABSTRACT_ROLES.len(), 12);
    for token in ABSTRACT_ROLES {
        assert_eq!(
            Role::parse_token(token),
            Err(RoleError::Abstract),
            "{token}"
        );
        assert_eq!(
            Role::parse_token(&token.to_uppercase()),
            Err(RoleError::Abstract)
        );
        assert_eq!(Role::parse_list(token), None, "{token}");
        // No variant carries an abstract token.
        assert!(Role::ALL.iter().all(|r| r.token() != token), "{token}");
    }
}

#[test]
fn a11y_role_tokens_round_trip_and_are_unique() {
    let mut seen = std::collections::BTreeSet::new();
    for role in Role::ALL {
        assert!(seen.insert(role.token()), "duplicate {}", role.token());
        if *role != Role::StaticText {
            assert_eq!(Role::parse_token(role.token()), Ok(*role));
        }
    }
    // ARIA 1.2 concrete roles, from its role index (`none` is a synonym of
    // `presentation`), plus the engine's text run.
    let aria_12 = [
        "alert",
        "alertdialog",
        "application",
        "article",
        "banner",
        "blockquote",
        "button",
        "caption",
        "cell",
        "checkbox",
        "code",
        "columnheader",
        "combobox",
        "complementary",
        "contentinfo",
        "definition",
        "deletion",
        "dialog",
        "directory",
        "document",
        "emphasis",
        "feed",
        "figure",
        "form",
        "generic",
        "grid",
        "gridcell",
        "group",
        "heading",
        "img",
        "insertion",
        "link",
        "list",
        "listbox",
        "listitem",
        "log",
        "main",
        "marquee",
        "math",
        "menu",
        "menubar",
        "menuitem",
        "menuitemcheckbox",
        "menuitemradio",
        "meter",
        "navigation",
        "note",
        "option",
        "paragraph",
        "presentation",
        "progressbar",
        "radio",
        "radiogroup",
        "region",
        "row",
        "rowgroup",
        "rowheader",
        "scrollbar",
        "search",
        "searchbox",
        "separator",
        "slider",
        "spinbutton",
        "status",
        "strong",
        "subscript",
        "superscript",
        "switch",
        "tab",
        "table",
        "tablist",
        "tabpanel",
        "term",
        "textbox",
        "time",
        "timer",
        "toolbar",
        "tooltip",
        "tree",
        "treegrid",
        "treeitem",
    ];
    let ours: std::collections::BTreeSet<&str> = Role::ALL
        .iter()
        .filter(|r| **r != Role::StaticText)
        .map(|r| r.token())
        .collect();
    let expected: std::collections::BTreeSet<&str> = aria_12.into_iter().collect();
    assert_eq!(ours, expected);
}

#[test]
fn a11y_role_fallback_list_is_bounded() {
    // 100,000 junk tokens and a real one at the end: bounded work, and the
    // role beyond the bound is not considered.
    let junk = "x ".repeat(100_000);
    assert_eq!(Role::parse_list(&format!("{junk}button")), None);
    assert_eq!(Role::parse_list("x x x button"), Some(Role::Button));
}

#[test]
fn a11y_role_name_from_follows_aria() {
    use NameFrom::{Author, Contents, Prohibited};
    let contents = [
        Role::Button,
        Role::Cell,
        Role::Checkbox,
        Role::ColumnHeader,
        Role::GridCell,
        Role::Heading,
        Role::Link,
        Role::MenuItem,
        Role::MenuItemCheckbox,
        Role::MenuItemRadio,
        Role::Option,
        Role::Radio,
        Role::Row,
        Role::RowHeader,
        Role::Switch,
        Role::Tab,
        Role::Tooltip,
        Role::TreeItem,
    ];
    for r in contents {
        assert_eq!(r.name_from(), Contents, "{r:?}");
    }
    let prohibited = [
        Role::Caption,
        Role::Code,
        Role::Deletion,
        Role::Emphasis,
        Role::Generic,
        Role::Insertion,
        Role::Paragraph,
        Role::Presentation,
        Role::Strong,
        Role::Subscript,
        Role::Superscript,
    ];
    for r in prohibited {
        assert_eq!(r.name_from(), Prohibited, "{r:?}");
    }
    for r in [Role::Dialog, Role::Navigation, Role::List, Role::TextBox] {
        assert_eq!(r.name_from(), Author, "{r:?}");
    }
}

#[test]
fn a11y_role_children_presentational_set() {
    for r in [
        Role::Button,
        Role::Checkbox,
        Role::Img,
        Role::Option,
        Role::Tab,
        Role::Slider,
    ] {
        assert!(r.children_presentational(), "{r:?}");
    }
    for r in [
        Role::Heading,
        Role::Link,
        Role::List,
        Role::Group,
        Role::Menu,
    ] {
        assert!(!r.children_presentational(), "{r:?}");
    }
}

#[test]
fn a11y_role_children_of_a_button_are_not_exposed() {
    let p = Page::new("<button id=b>Save <span id=s>now</span> <img id=i alt=icon></button>");
    assert_eq!(p.lines(), "button name=\"Save now icon\" [focusable]\n");
    assert_eq!(p.role("s"), "-");
    assert_eq!(p.role("i"), "-");
}
