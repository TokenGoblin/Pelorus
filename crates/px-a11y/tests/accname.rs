//! Accessible name and description computation.
//!
//! The cases marked "accname" are the examples printed in the Accessible Name
//! and Description Computation 1.2 text (the `w3c/aria` repository's
//! `accname/index.html`), translated to HTML; the cases marked "HTML-AAM" come
//! from the "Accessible Name and Description Computation" tables of HTML-AAM.
//! Steps are named as the specification names them (Hidden Not Referenced,
//! LabelledBy, Embedded Control, AriaLabel, Host Language Label, Name From
//! Content, Text Node, Recursive Name From Content, Tooltip) and exercised in
//! that order.

mod common;

use common::{Page, name_of};

fn names(html: &str, ids: &[&str]) -> Vec<String> {
    let p = Page::new(html);
    ids.iter().map(|i| p.name(i)).collect()
}

// --- Initialisation ----------------------------------------------------

#[test]
fn a11y_accname_prohibited_roles_have_no_name() {
    // A paragraph (role paragraph) cannot be named, whatever the author wrote.
    let p = Page::new("<p id=x aria-label=n title=t>text</p>");
    assert_eq!(p.name("x"), "");
    // The title cannot name it, so it describes it.
    assert_eq!(p.lines(), "paragraph description=\"t\"\n  text \"text\"\n");
    // Same for an explicit presentational/generic-prohibited role.
    assert_eq!(
        name_of("<div id=x role=code aria-label=n>c</div>"),
        "",
        "code is a name-prohibited role"
    );
    assert_eq!(
        name_of("<strong id=x aria-labelledby=y>c</strong><i id=y>no</i>"),
        ""
    );
}

// --- 2A Hidden Not Referenced ------------------------------------------

#[test]
fn a11y_accname_hidden_nodes_do_not_contribute() {
    assert_eq!(
        name_of("<button id=x>a<span hidden>b</span>c</button>"),
        "ac"
    );
    assert_eq!(
        name_of("<button id=x>a<span aria-hidden=true>b</span>c</button>"),
        "ac"
    );
    // aria-hidden="false" does not hide.
    assert_eq!(
        name_of("<button id=x>a<span aria-hidden=false>b</span>c</button>"),
        "abc"
    );
    // A hidden ancestor hides the descendants.
    assert_eq!(
        name_of("<button id=x>a<span hidden><b>b</b></span>c</button>"),
        "ac"
    );
}

#[test]
fn a11y_accname_spec_example_hidden_referenced_subtree_is_read_in_full() {
    // accname 2A, first example: element3 is hidden, but it is part of an
    // aria-labelledby traversal started in element2, which is hidden too.
    // element1's name is "hello".
    assert_eq!(
        name_of(
            "<div id=x role=button aria-labelledby=el2></div>\
             <div id=el2 hidden><div id=el3 hidden>hello</div></div>"
        ),
        "hello"
    );
}

#[test]
fn a11y_accname_spec_example_hidden_inside_visible_referenced_node_is_skipped() {
    // accname 2A, second example: element2 is not hidden, element3 is hidden:
    // element1 has no accessible name.
    assert_eq!(
        name_of(
            "<div id=x role=button aria-labelledby=el2></div>\
             <div id=el2><div id=el3 hidden>hello</div></div>"
        ),
        ""
    );
}

#[test]
fn a11y_accname_a_hidden_node_named_by_aria_hidden_is_still_read_when_referenced() {
    assert_eq!(
        name_of(
            "<span id=h aria-hidden=true>secret</span><button id=x aria-labelledby=h>x</button>"
        ),
        "secret"
    );
}

// --- 2B LabelledBy -------------------------------------------------------

#[test]
fn a11y_accname_labelledby_concatenates_in_idref_order_with_spaces() {
    let html = "<span id=a>one</span><span id=b>two</span><span id=c>three</span>\
                <button id=x aria-labelledby='c a b'>content</button>";
    assert_eq!(name_of(html), "three one two");
}

#[test]
fn a11y_accname_labelledby_ignores_unresolvable_ids_but_needs_one_valid() {
    assert_eq!(
        name_of("<span id=a>A</span><button id=x aria-labelledby='nope a also-nope'>c</button>"),
        "A"
    );
    // No valid IDREF at all: the attribute is not "applicable", fall through.
    assert_eq!(
        name_of("<button id=x aria-labelledby='nope' aria-label=L>c</button>"),
        "L"
    );
    assert_eq!(
        name_of("<button id=x aria-labelledby=''>content</button>"),
        "content"
    );
}

#[test]
fn a11y_accname_labelledby_wins_over_aria_label_when_it_yields_text() {
    assert_eq!(
        name_of("<span id=a>A</span><button id=x aria-labelledby=a aria-label=L>c</button>"),
        "A"
    );
    // ...but an empty result falls through to aria-label.
    assert_eq!(
        name_of("<span id=a></span><button id=x aria-labelledby=a aria-label=L>c</button>"),
        "L"
    );
}

#[test]
fn a11y_accname_spec_example_labelledby_is_not_followed_inside_a_traversal() {
    // accname 2B example: element1 labelledby element3; element2 labelledby
    // element1. element1's name is "hello"; element2 has no name, because
    // element1's own aria-labelledby is not followed during element2's
    // traversal and element1 has no other source.
    let html = "<div id=el1 role=button aria-labelledby=el3></div>\
                <div id=x role=button aria-labelledby=el1></div>\
                <div id=el3> hello </div>";
    let p = Page::new(html);
    assert_eq!(p.name("el1"), "hello");
    assert_eq!(p.name("x"), "");
}

#[test]
fn a11y_accname_spec_example_self_reference_uses_aria_label() {
    // accname 2C example: the delete buttons name themselves from their own
    // aria-label plus the file name.
    let html = "<h1>Files</h1><ul>\
        <li><a id=file_row1 href=Documentation.pdf>Documentation.pdf</a>\
        <span role=button tabindex=0 id=del_row1 aria-label=Delete aria-labelledby='del_row1 file_row1'></span></li>\
        <li><a id=file_row2 href=HolidayLetter.pdf>HolidayLetter.pdf</a>\
        <span role=button tabindex=0 id=del_row2 aria-label=Delete aria-labelledby='del_row2 file_row2'></span></li>\
        </ul>";
    let p = Page::new(html);
    assert_eq!(p.name("del_row1"), "Delete Documentation.pdf");
    assert_eq!(p.name("del_row2"), "Delete HolidayLetter.pdf");
}

#[test]
fn a11y_accname_labelledby_cycles_terminate() {
    // a -> b -> a. Each reads the other's content; neither loops.
    let p = Page::new(
        "<span id=a role=button aria-labelledby=b>A</span>\
         <span id=b role=button aria-labelledby=a>B</span>",
    );
    assert_eq!(p.name("a"), "B");
    assert_eq!(p.name("b"), "A");
    // Self reference with no other source reads its own content once.
    assert_eq!(
        name_of("<div id=x role=button aria-labelledby=x>self</div>"),
        "self"
    );
    // A ring of 50.
    let mut html = String::new();
    for i in 0..50 {
        html.push_str(&format!(
            "<span id=n{i} role=button aria-labelledby=n{}>t{i}</span>",
            (i + 1) % 50
        ));
    }
    let p = Page::new(&html);
    assert_eq!(p.name("n0"), "t1");
    assert_eq!(p.name("n49"), "t0");
}

#[test]
fn a11y_accname_each_node_is_consulted_once() {
    // accname 2F: "Each node in the subtree is consulted only once". The same
    // id twice contributes once.
    assert_eq!(
        name_of("<span id=t>hi</span><button id=x aria-labelledby='t t'>c</button>"),
        "hi"
    );
    // An element and its own child both listed: the child was already read
    // as part of the parent.
    assert_eq!(
        name_of("<div id=p>a <b id=c>b</b></div><button id=x aria-labelledby='p c'>c</button>"),
        "a b"
    );
}

#[test]
fn a11y_accname_labelledby_huge_idref_lists_are_bounded() {
    let ids = (0..50_000)
        .map(|i| format!("i{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    let p = Page::new(&format!(
        "<span id=i0>zero</span><span id=i63>sixty-three</span><span id=i64>late</span>\
         <button id=x aria-labelledby='{ids}'>c</button>"
    ));
    // Only the first 64 tokens are considered.
    assert_eq!(p.name("x"), "zero sixty-three");
}

// --- 2E Embedded Control -------------------------------------------------

#[test]
fn a11y_accname_spec_example_embedded_textbox() {
    // accname 2E example: "Flash the screen [input] times", the input holds 5.
    let html = "<label for=flash><input type=checkbox id=flash> Flash the screen \
                <span tabindex=0 role=textbox aria-label='number of times' contenteditable>5</span> \
                times.</label>";
    assert_eq!(Page::new(html).name("flash"), "Flash the screen 5 times.");
}

#[test]
fn a11y_accname_embedded_native_controls() {
    let p = Page::new(
        "<label id=l1 for=c1>Volume <input type=range value=7> loud</label><input id=c1 type=checkbox>\
         <label for=c2>Pick <select><option>First</option><option selected>Second</option></select> now</label>\
         <input id=c2 type=checkbox>\
         <label for=c3>Say <input type=text value='hello there'> twice</label><input id=c3 type=checkbox>",
    );
    assert_eq!(p.name("c1"), "Volume 7 loud");
    assert_eq!(p.name("c2"), "Pick Second now");
    assert_eq!(p.name("c3"), "Say hello there twice");
}

#[test]
fn a11y_accname_embedded_range_prefers_valuetext_then_valuenow() {
    let html = "<label for=c>A <span role=slider aria-valuenow=3 aria-valuetext='three'></span> B\
                <span role=slider aria-valuenow=4></span></label><input id=c type=checkbox>";
    assert_eq!(Page::new(html).name("c"), "A three B4");
}

#[test]
fn a11y_accname_password_values_are_not_leaked_into_names() {
    let html =
        "<label for=c>Pin <input type=password value=hunter2></label><input id=c type=checkbox>";
    let p = Page::new(html);
    assert!(!p.name("c").contains("hunter2"), "{}", p.name("c"));
    assert!(!p.tree.dump().contains("hunter2"));
}

// --- 2C AriaLabel ---------------------------------------------------------

#[test]
fn a11y_accname_aria_label_beats_native_markup_and_content() {
    assert_eq!(
        name_of("<button id=x aria-label=L title=T>content</button>"),
        "L"
    );
    assert_eq!(name_of("<img id=x aria-label=L alt=A>"), "L");
    assert_eq!(
        name_of("<label for=x>Label</label><input id=x aria-label=L>"),
        "L"
    );
}

#[test]
fn a11y_accname_empty_or_blank_aria_label_is_skipped() {
    assert_eq!(
        name_of("<button id=x aria-label='' >content</button>"),
        "content"
    );
    assert_eq!(
        name_of("<button id=x aria-label='   \n'>content</button>"),
        "content"
    );
    assert_eq!(
        name_of("<button id=x aria-label='  padded  '>content</button>"),
        "padded"
    );
}

// --- 2D Host Language Label (HTML-AAM) ----------------------------------------

#[test]
fn a11y_accname_html_aam_text_input_label_order() {
    // aria -> label -> title -> placeholder.
    assert_eq!(
        name_of("<label for=x>Name</label><input id=x title=T placeholder=P>"),
        "Name"
    );
    assert_eq!(name_of("<input id=x title=T placeholder=P>"), "T");
    assert_eq!(name_of("<input id=x placeholder=P>"), "P");
    assert_eq!(name_of("<input id=x>"), "");
    assert_eq!(name_of("<textarea id=x placeholder=P></textarea>"), "P");
    assert_eq!(name_of("<input id=x aria-placeholder=AP>"), "AP");
}

#[test]
fn a11y_accname_html_aam_label_association() {
    // for=, wrapping (first labelable descendant), several labels in DOM order.
    assert_eq!(name_of("<label for=x>A</label><input id=x>"), "A");
    assert_eq!(name_of("<label>Wrap <input id=x></label>"), "Wrap");
    assert_eq!(
        name_of("<label for=x>A</label><input id=x><label for=x>B</label>"),
        "A B"
    );
    assert_eq!(
        name_of("<label>W<input id=x></label><label for=x>B</label>"),
        "W B"
    );
    // `for` naming something that is not labelable labels nothing.
    assert_eq!(
        name_of("<label for=d>A</label><div id=d></div><input id=x>"),
        ""
    );
    // A wrapping label labels only the first labelable descendant.
    let p = Page::new("<label>L <input id=a><input id=b></label>");
    assert_eq!(p.name("a"), "L");
    assert_eq!(p.name("b"), "");
    // An encapsulated control's own value is excluded from its name.
    assert_eq!(
        name_of("<label>Name <input id=x value=typed></label>"),
        "Name"
    );
}

#[test]
fn a11y_accname_html_aam_label_applies_to_all_labelable_elements() {
    for el in [
        "<select id=x><option>o</option></select>",
        "<textarea id=x></textarea>",
        "<meter id=x value=1></meter>",
        "<progress id=x value=1></progress>",
        "<output id=x></output>",
        "<button id=x></button>",
        "<input id=x type=checkbox>",
        "<input id=x type=radio>",
    ] {
        assert_eq!(
            name_of(&format!("<label for=x>Lbl</label>{el}")),
            "Lbl",
            "{el}"
        );
    }
}

#[test]
fn a11y_accname_html_aam_buttons() {
    assert_eq!(name_of("<input id=x type=button value=Go>"), "Go");
    assert_eq!(name_of("<input id=x type=submit value=Send>"), "Send");
    // Implementation-defined strings when no value (untranslated here).
    assert_eq!(name_of("<input id=x type=submit>"), "Submit");
    assert_eq!(name_of("<input id=x type=reset>"), "Reset");
    assert_eq!(name_of("<input id=x type=button>"), "");
    // title only after value.
    assert_eq!(name_of("<input id=x type=button title=T>"), "T");
    // A label beats value for a button input.
    assert_eq!(
        name_of("<label for=x>Lbl</label><input id=x type=button value=V>"),
        "Lbl"
    );
    // button element: label first, then subtree, then title.
    assert_eq!(
        name_of("<label for=x>Lbl</label><button id=x>Content</button>"),
        "Lbl"
    );
    assert_eq!(name_of("<button id=x>Content</button>"), "Content");
    assert_eq!(name_of("<button id=x title=T></button>"), "T");
}

#[test]
fn a11y_accname_html_aam_input_image() {
    assert_eq!(name_of("<input id=x type=image alt=Search>"), "Search");
    assert_eq!(name_of("<input id=x type=image alt='  ' title=T>"), "T");
    assert_eq!(name_of("<input id=x type=image>"), "Submit");
}

#[test]
fn a11y_accname_html_aam_img() {
    assert_eq!(name_of("<img id=x alt=Alt title=T>"), "Alt");
    assert_eq!(name_of("<img id=x title=T>"), "T");
    assert_eq!(name_of("<img id=x>"), "");
    // alt="" with nothing else is presentational: not in the tree at all.
    assert_eq!(name_of("<img id=x alt=''>"), "<absent>");
    // figcaption names a lone image (HTML-AAM `img`).
    assert_eq!(
        name_of("<figure><img id=x><figcaption>Caption</figcaption></figure>"),
        "Caption"
    );
    // ...but not when other content shares the figure,
    assert_eq!(
        name_of("<figure><img id=x><p>other</p><figcaption>Caption</figcaption></figure>"),
        ""
    );
    // or when the image has its own alt.
    assert_eq!(
        name_of("<figure><img id=x alt=Own><figcaption>Caption</figcaption></figure>"),
        "Own"
    );
}

#[test]
fn a11y_accname_html_aam_figure_name_does_not_come_from_figcaption() {
    // HTML-AAM `figure`: the figcaption "does not take part in the accessible
    // name ... unless explicitly referenced".
    assert_eq!(
        name_of("<figure id=x><figcaption>Cap</figcaption></figure>"),
        ""
    );
    assert_eq!(
        name_of("<figure id=x aria-labelledby=c><figcaption id=c>Cap</figcaption></figure>"),
        "Cap"
    );
}

#[test]
fn a11y_accname_html_aam_fieldset_and_table() {
    assert_eq!(
        name_of("<fieldset id=x><legend>Legend</legend><input></fieldset>"),
        "Legend"
    );
    assert_eq!(
        name_of("<fieldset id=x title=T><legend>Legend</legend></fieldset>"),
        "Legend"
    );
    assert_eq!(name_of("<fieldset id=x title=T></fieldset>"), "T");
    assert_eq!(
        name_of("<fieldset id=x aria-label=L><legend>Legend</legend></fieldset>"),
        "L"
    );
    assert_eq!(
        name_of("<table id=x><caption>Cap</caption><tr><td>c</td></tr></table>"),
        "Cap"
    );
    assert_eq!(
        name_of("<table id=x title=T><tr><td>c</td></tr></table>"),
        "T"
    );
}

#[test]
fn a11y_accname_html_aam_option_and_optgroup_labels() {
    let p = Page::new(
        "<select><optgroup id=g label=Group><option id=o1 label=Short>Long text</option>\
         <option id=o2>Plain</option></optgroup></select>",
    );
    assert_eq!(p.name("g"), "Group");
    assert_eq!(p.name("o1"), "Short");
    assert_eq!(p.name("o2"), "Plain");
}

#[test]
fn a11y_accname_html_aam_summary_and_details() {
    let p = Page::new("<details open><summary id=x>More <b>info</b></summary>body</details>");
    assert_eq!(p.name("x"), "More info");
}

#[test]
fn a11y_accname_html_aam_link_uses_subtree_then_title() {
    assert_eq!(name_of("<a id=x href=u>Go <b>now</b></a>"), "Go now");
    assert_eq!(name_of("<a id=x href=u title=T>Go</a>"), "Go");
    assert_eq!(name_of("<a id=x href=u title=T></a>"), "T");
    // A link containing only an image is named by the image's alt.
    assert_eq!(name_of("<a id=x href=u><img alt=Home></a>"), "Home");
}

// --- 2F / 2G / 2H Name From Content ----------------------------------------------

#[test]
fn a11y_accname_name_from_content_joins_inline_text_without_added_space() {
    assert_eq!(
        name_of("<h1 id=x>Hello <span>World</span></h1>"),
        "Hello World"
    );
    assert_eq!(name_of("<h1 id=x><b>a</b><i>b</i></h1>"), "ab");
    assert_eq!(name_of("<h1 id=x>a<b> b </b>c</h1>"), "a b c");
}

#[test]
fn a11y_accname_name_from_content_separates_block_children() {
    assert_eq!(
        name_of("<div id=x role=button>one<div>two</div>three</div>"),
        "one two three"
    );
    assert_eq!(name_of("<div id=x role=link>a<br>b</div>"), "a b");
}

#[test]
fn a11y_accname_name_from_content_flattens_whitespace() {
    // "Flat string": newlines, tabs and form feeds become one space; runs
    // collapse; ends are trimmed. A no-break space is not collapsed.
    assert_eq!(
        name_of("<button id=x>\n  a \t\t b\x0c\x0c c\r\n</button>"),
        "a b c"
    );
    assert_eq!(
        name_of("<button id=x>a&nbsp;&nbsp;b</button>"),
        "a\u{a0}\u{a0}b"
    );
}

#[test]
fn a11y_accname_roles_that_do_not_allow_name_from_content_have_no_content_name() {
    // `region`/`navigation` are name-from-author: content is not used.
    assert_eq!(name_of("<nav id=x>Menu</nav>"), "");
    assert_eq!(name_of("<ul id=x><li>a</li></ul>"), "");
    // ...but they can be named by reference to content.
    assert_eq!(
        name_of("<nav id=x aria-labelledby=h>x</nav><h2 id=h>Menu</h2>"),
        "Menu"
    );
}

#[test]
fn a11y_accname_nested_names_use_each_descendants_own_higher_precedence_sources() {
    // 2H: a descendant's text alternative can come from aria-label.
    assert_eq!(
        name_of("<button id=x>A <span aria-label=inner>ignored</span> B</button>"),
        "A inner B"
    );
    assert_eq!(
        name_of("<button id=x>A <img alt=pic> B</button>"),
        "A pic B"
    );
}

#[test]
fn a11y_accname_row_name_comes_from_cells() {
    let p = Page::new("<table><tr id=x><td>a</td><td>b</td></tr></table>");
    assert_eq!(p.name("x"), "a b");
}

// --- 2I Tooltip ------------------------------------------------------------------

#[test]
fn a11y_accname_tooltip_is_the_last_resort() {
    assert_eq!(name_of("<button id=x title=T>content</button>"), "content");
    assert_eq!(name_of("<button id=x title=T></button>"), "T");
    assert_eq!(name_of("<button id=x title='  '></button>"), "");
    assert_eq!(name_of("<div id=x role=button title=T></div>"), "T");
}

// --- Description -------------------------------------------------------------------

#[test]
fn a11y_accname_description_precedence() {
    // 1. aria-describedby, 2. aria-description, 3. host language, 4. title.
    let p = Page::new(
        "<span id=d>by ref</span>\
         <button id=a aria-describedby=d aria-description=attr title=T>A</button>\
         <button id=b aria-description=attr title=T>B</button>\
         <button id=c title=T>C</button>\
         <button id=e>E</button>",
    );
    assert_eq!(p.desc("a"), "by ref");
    assert_eq!(p.desc("b"), "attr");
    // title is the name's tooltip only when nothing else named it; here the
    // content names it, so the title is the description.
    assert_eq!(p.desc("c"), "T");
    assert_eq!(p.desc("e"), "");
}

#[test]
fn a11y_accname_title_is_not_both_name_and_description() {
    let p = Page::new("<button id=a title=T></button><img id=b title=Pic>");
    assert_eq!(p.name("a"), "T");
    assert_eq!(p.desc("a"), "");
    assert_eq!(p.name("b"), "Pic");
    assert_eq!(p.desc("b"), "");
}

#[test]
fn a11y_accname_describedby_follows_hidden_refs_and_shares_the_traversal_rules() {
    let p = Page::new(
        "<div id=d1 hidden>hidden <b>text</b></div><div id=d2>two</div>\
         <button id=x aria-describedby='d1 d2 d1'>X</button>",
    );
    assert_eq!(p.desc("x"), "hidden text two");
    // A describedby target that itself has aria-labelledby: not followed.
    let p = Page::new(
        "<span id=t>T</span><div id=d aria-labelledby=t>own</div>\
         <button id=x aria-describedby=d>X</button>",
    );
    assert_eq!(p.desc("x"), "own");
}

#[test]
fn a11y_accname_html_aam_description_from_table_caption_and_button_value() {
    let p = Page::new(
        "<table id=t aria-label=Named><caption>Cap text</caption><tr><td>c</td></tr></table>\
         <table id=u><caption>Cap</caption><tr><td>c</td></tr></table>\
         <input id=b type=submit value=Go aria-label=Label>\
         <input id=c type=submit value=Go>",
    );
    assert_eq!(p.desc("t"), "Cap text");
    // The caption already named table u: not repeated.
    assert_eq!(p.name("u"), "Cap");
    assert_eq!(p.desc("u"), "");
    assert_eq!(p.desc("b"), "Go");
    assert_eq!(p.desc("c"), "");
}

// --- Hostile values ------------------------------------------------------------

#[test]
fn a11y_accname_names_are_capped_and_sanitised() {
    let long = "x".repeat(1 << 20);
    let p = Page::new(&format!("<button id=x aria-label='{long}'>b</button>"));
    assert_eq!(p.name("x").len(), px_a11y::MAX_STRING);
    // Multi-byte characters are cut on a boundary, never inside one.
    let wide = "\u{1F600}".repeat(5000);
    let p = Page::new(&format!("<button id=x aria-label='{wide}'>b</button>"));
    let n = p.name("x");
    assert!(n.len() <= px_a11y::MAX_STRING);
    assert!(n.chars().all(|c| c == '\u{1F600}'));
    // NUL becomes U+FFFD: a NUL would end the string in a C accessibility API.
    let p = Page::new("<button id=x aria-label='a&#0;b'>b</button>");
    assert!(!p.name("x").contains('\0'));
}

#[test]
fn a11y_accname_unicode_is_preserved() {
    assert_eq!(
        name_of("<button id=x>\u{202e}rtl \u{65}\u{301} \u{4e2d}\u{6587}</button>"),
        "\u{202e}rtl e\u{301} \u{4e2d}\u{6587}"
    );
}

#[test]
fn a11y_accname_work_is_bounded_for_nested_names() {
    // 500 nested buttons-as-roles: each name walks its subtree; the total is
    // bounded, and the process neither hangs nor overflows.
    let mut html = String::new();
    for _ in 0..500 {
        html.push_str("<div role=link>x");
    }
    for _ in 0..500 {
        html.push_str("</div>");
    }
    let p = Page::new(&html);
    assert!(!p.tree.is_empty());
}

#[test]
fn a11y_accname_document_name_is_the_title() {
    let p = Page::new("<title>  My   Page </title><p>x</p>");
    assert!(
        p.tree.dump().starts_with("document name=\"My Page\"\n"),
        "{}",
        p.tree.dump()
    );
    let p = Page::new("<p>x</p>");
    assert!(p.tree.dump().starts_with("document\n"));
    let _ = names;
}
