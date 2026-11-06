//! Static SVG rendering: exact pixels on small documents, the safety rules
//! (nothing is loaded, run or expanded), and hostile input.
#![cfg(test)]

mod common;
use common::*;
use px_image::svg::{Document, decode as svg_decode};
use px_image::{DecodeError, Decoded, Format, Image, Limits, decode, sniff};

const NS: &str = "xmlns='http://www.w3.org/2000/svg'";

fn render(svg: &str) -> Image {
    svg_decode(svg.as_bytes(), &Limits::DEFAULT).unwrap()
}

fn px(img: &Image, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * img.width + x) * 4) as usize;
    [
        img.rgba[i],
        img.rgba[i + 1],
        img.rgba[i + 2],
        img.rgba[i + 3],
    ]
}

#[test]
fn sniffed_and_decoded_through_the_common_entry_point() {
    let s =
        format!("<svg {NS} width='8' height='4'><rect width='8' height='4' fill='#0f0'/></svg>");
    assert_eq!(sniff(s.as_bytes()), Some(Format::Svg));
    match decode(s.as_bytes(), &Limits::DEFAULT).unwrap() {
        Decoded::Still(i) => {
            assert_eq!((i.width, i.height), (8, 4));
            assert_eq!(px(&i, 3, 2), [0, 255, 0, 255]);
        }
        Decoded::Animated(_) => panic!("svg is a still"),
    }
    // Other formats and plain text are not SVG.
    assert_eq!(sniff(b"<html><body>hello</body></html>"), None);
    assert_eq!(sniff(b"hello <svg>"), None);
    assert_eq!(
        sniff(b"\xEF\xBB\xBF\n <?xml version='1.0'?><svg/>"),
        Some(Format::Svg)
    );
}

#[test]
fn pixel_aligned_shapes_are_exact() {
    let img = render(&format!(
        "<svg {NS} width='10' height='10'><rect x='2' y='2' width='4' height='4' fill='red'/></svg>"
    ));
    assert_eq!(px(&img, 1, 1), [0, 0, 0, 0]);
    assert_eq!(px(&img, 2, 2), [255, 0, 0, 255]);
    assert_eq!(px(&img, 5, 5), [255, 0, 0, 255]);
    assert_eq!(px(&img, 6, 6), [0, 0, 0, 0]);
}

#[test]
fn edges_are_anti_aliased_by_coverage() {
    // Half a pixel in from the left: the first column is half covered.
    let img = render(&format!(
        "<svg {NS} width='4' height='2'><rect x='0.5' width='3' height='2' fill='#000'/></svg>"
    ));
    assert_eq!(px(&img, 0, 0), [0, 0, 0, 128]);
    assert_eq!(px(&img, 1, 0), [0, 0, 0, 255]);
    assert_eq!(px(&img, 3, 0), [0, 0, 0, 128]);
}

#[test]
fn view_box_and_aspect_ratio_place_the_picture() {
    // 10x10 box in a 20x10 image: meet, centred, so x runs 5..15.
    let img = render(&format!(
        "<svg {NS} width='20' height='10' viewBox='0 0 10 10'><rect width='10' height='10'/></svg>"
    ));
    assert_eq!(px(&img, 4, 5)[3], 0);
    assert_eq!(px(&img, 5, 5)[3], 255);
    assert_eq!(px(&img, 14, 5)[3], 255);
    assert_eq!(px(&img, 15, 5)[3], 0);
    let img = render(&format!(
        "<svg {NS} width='20' height='10' viewBox='0 0 10 10' preserveAspectRatio='none'><rect width='10' height='10'/></svg>"
    ));
    assert_eq!(px(&img, 0, 0)[3], 255);
    assert_eq!(px(&img, 19, 9)[3], 255);
}

#[test]
fn intrinsic_size_rules() {
    let size = |attrs: &str| {
        Document::parse(format!("<svg {NS} {attrs}/>").as_bytes())
            .unwrap()
            .intrinsic_size()
    };
    assert_eq!(size("width='40' height='30'"), (40, 30));
    assert_eq!(size("width='1in' height='0.5in'"), (96, 48));
    assert_eq!(size("viewBox='0 0 10 10'"), (150, 150));
    assert_eq!(size("viewBox='0 0 20 5'"), (300, 75));
    assert_eq!(size("width='60' viewBox='0 0 20 10'"), (60, 30));
    assert_eq!(size("height='60' viewBox='0 0 20 10'"), (120, 60));
    assert_eq!(size(""), (300, 150));
    assert_eq!(size("width='50%' height='50%'"), (300, 150));
}

#[test]
fn fill_stroke_opacity_and_current_color() {
    let img = render(&format!(
        "<svg {NS} width='6' height='6'><rect width='6' height='6' fill='#f00' fill-opacity='0.5'/></svg>"
    ));
    assert_eq!(px(&img, 3, 3), [255, 0, 0, 128]);
    // Group opacity composites once, not per shape: overlapping children of an
    // opacity group do not show through each other.
    let img = render(&format!(
        "<svg {NS} width='6' height='6'><g opacity='0.5'><rect width='6' height='6' fill='#000'/><rect width='6' height='6' fill='#fff'/></g></svg>"
    ));
    assert_eq!(px(&img, 3, 3), [255, 255, 255, 128]);
    // currentColor is whatever the embedder says.
    let doc = Document::parse(
        format!(
            "<svg {NS} width='2' height='2'><rect width='2' height='2' fill='currentColor'/></svg>"
        )
        .as_bytes(),
    )
    .unwrap();
    let img = doc
        .render(2, 2, [10, 20, 30, 255], &Limits::DEFAULT)
        .unwrap();
    assert_eq!(px(&img, 1, 1), [10, 20, 30, 255]);
    // A stroke is centred on the edge.
    let img = render(&format!(
        "<svg {NS} width='10' height='10'><rect x='2' y='2' width='6' height='6' fill='none' stroke='#00f' stroke-width='2'/></svg>"
    ));
    assert_eq!(px(&img, 1, 5), [0, 0, 255, 255]);
    assert_eq!(px(&img, 2, 5), [0, 0, 255, 255]);
    assert_eq!(px(&img, 3, 5), [0, 0, 0, 0]);
    assert_eq!(px(&img, 5, 5), [0, 0, 0, 0]);
}

#[test]
fn fill_rules() {
    let d = "M0 0 H8 V8 H0 Z M2 2 H6 V6 H2 Z";
    let nz = render(&format!(
        "<svg {NS} width='8' height='8'><path d='{d}'/></svg>"
    ));
    let eo = render(&format!(
        "<svg {NS} width='8' height='8'><path d='{d}' fill-rule='evenodd'/></svg>"
    ));
    assert_eq!(px(&nz, 4, 4)[3], 255);
    assert_eq!(px(&eo, 4, 4)[3], 0);
    assert_eq!(px(&eo, 1, 1)[3], 255);
}

#[test]
fn gradients_run_from_first_to_last_stop() {
    let img = render(&format!(
        "<svg {NS} width='100' height='2'><defs><linearGradient id='g'><stop offset='0' stop-color='#000'/><stop offset='1' stop-color='#fff'/></linearGradient></defs>\
         <rect width='100' height='2' fill='url(#g)'/></svg>"
    ));
    let (a, b, c) = (px(&img, 0, 0)[0], px(&img, 50, 0)[0], px(&img, 99, 0)[0]);
    assert!(a < 5 && c > 250, "{a} {c}");
    assert!((120..136).contains(&b), "{b}");
    // A gradient that points at itself, or at nothing, is not a crash.
    let img = render(&format!(
        "<svg {NS} width='4' height='4'><linearGradient id='a' href='#b'/><linearGradient id='b' href='#a'/>\
         <rect width='4' height='4' fill='url(#a) #0f0'/><rect width='2' height='2' fill='url(#missing)'/></svg>"
    ));
    assert_eq!(px(&img, 3, 3), [0, 255, 0, 255]);
    assert_eq!(px(&img, 0, 0), [0, 255, 0, 255]);
}

#[test]
fn use_clip_and_style_work_inside_the_document() {
    let img = render(&format!(
        "<svg {NS} width='10' height='4'><style>.c {{ fill: #f00 }}</style><defs><rect id='r' class='c' width='4' height='4'/>\
         <clipPath id='k'><rect width='2' height='4'/></clipPath></defs>\
         <use href='#r' x='0'/><use href='#r' transform='translate(6 0)' clip-path='url(#k)'/></svg>"
    ));
    assert_eq!(px(&img, 1, 1), [255, 0, 0, 255]);
    assert_eq!(px(&img, 6, 1), [255, 0, 0, 255]);
    assert_eq!(px(&img, 7, 1), [255, 0, 0, 255]);
    assert_eq!(px(&img, 8, 1), [0, 0, 0, 0]);
}

#[test]
fn rendering_is_deterministic_and_size_independent_of_state() {
    let src = include_str!("../../../tests/render/pages/img/paths.svg");
    let doc = Document::parse(src.as_bytes()).unwrap();
    let a = doc
        .render(200, 160, [0, 0, 0, 255], &Limits::DEFAULT)
        .unwrap();
    let b = doc
        .render(200, 160, [0, 0, 0, 255], &Limits::DEFAULT)
        .unwrap();
    assert_eq!(a, b);
    let c = Document::parse(src.as_bytes())
        .unwrap()
        .render(200, 160, [0, 0, 0, 255], &Limits::DEFAULT)
        .unwrap();
    assert_eq!(a, c);
    // Scaled renders are valid too.
    let big = doc
        .render(400, 320, [0, 0, 0, 255], &Limits::DEFAULT)
        .unwrap();
    assert_eq!(big.rgba.len(), 400 * 320 * 4);
}

// -- safety -------------------------------------------------------------------

#[test]
fn nothing_external_is_ever_drawn_or_followed() {
    let plain = render(&format!(
        "<svg {NS} width='20' height='20'><rect width='5' height='5'/></svg>"
    ));
    let with_junk = render(&format!(
        "<svg {NS} xmlns:xlink='http://www.w3.org/1999/xlink' width='20' height='20' onload='x()'>\
         <script>boom()</script><image href='http://evil.test/a.png' width='20' height='20'/>\
         <image xlink:href='data:image/png;base64,AAAA' width='20' height='20'/>\
         <foreignObject width='20' height='20'><div xmlns='http://www.w3.org/1999/xhtml'>x</div></foreignObject>\
         <use href='http://evil.test/a.svg#p'/><use xlink:href='file:///etc/passwd#p'/>\
         <animate attributeName='width' to='20'/><set attributeName='fill' to='red'/>\
         <rect width='5' height='5' onclick='x()' fill='url(http://evil.test/p.svg#g)' style='stroke: url(\"//evil.test/#s\")'/></svg>"
    ));
    // The only difference is the rect's fill: an external paint reference with
    // no fallback paints nothing, so the rect disappears; everything else is
    // inert. No alpha anywhere else.
    assert!(with_junk.rgba.chunks(4).all(|p| p[3] == 0));
    assert!(plain.rgba.chunks(4).any(|p| p[3] != 0));
    // `@import` and external `url()` in a stylesheet do nothing.
    let img = render(&format!(
        "<svg {NS} width='4' height='4'><style>@import url(http://evil.test/x.css); rect {{ fill: #0f0; background: url(http://evil.test/y.png) }}</style><rect width='4' height='4'/></svg>"
    ));
    assert_eq!(px(&img, 2, 2), [0, 255, 0, 255]);
}

#[test]
fn entities_are_not_expanded() {
    let bomb = "<?xml version='1.0'?><!DOCTYPE svg [<!ENTITY a 'aaaaaaaaaa'><!ENTITY b '&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;'>\
        <!ENTITY c '&b;&b;&b;&b;&b;&b;&b;&b;&b;&b;'><!ENTITY d '&c;&c;&c;&c;&c;&c;&c;&c;&c;&c;'>]>\
        <svg xmlns='http://www.w3.org/2000/svg' width='2' height='2'><desc>&d;</desc></svg>";
    assert!(matches!(
        svg_decode(bomb.as_bytes(), &Limits::DEFAULT),
        Err(DecodeError::Malformed(_))
    ));
    let xxe = "<!DOCTYPE svg [<!ENTITY x SYSTEM 'file:///etc/passwd'>]>\
        <svg xmlns='http://www.w3.org/2000/svg' width='2' height='2'><title>&x;</title></svg>";
    assert!(svg_decode(xxe.as_bytes(), &Limits::DEFAULT).is_err());
    // The doctype alone is skipped.
    let ok = "<!DOCTYPE svg PUBLIC '-//W3C//DTD SVG 1.1//EN' 'http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd'>\
        <svg xmlns='http://www.w3.org/2000/svg' width='2' height='2'/>";
    assert!(svg_decode(ok.as_bytes(), &Limits::DEFAULT).is_ok());
}

#[test]
fn use_bombs_cycles_and_depth_are_bounded() {
    // Exponential use tree: 10^6 leaves.
    let mut s =
        format!("<svg {NS} width='4' height='4'><defs><rect id='l0' width='1' height='1'/>");
    for i in 1..=6 {
        s.push_str(&format!("<g id='l{i}'>"));
        for _ in 0..10 {
            s.push_str(&format!("<use href='#l{}'/>", i - 1));
        }
        s.push_str("</g>");
    }
    s.push_str("</defs><use href='#l6'/></svg>");
    assert!(matches!(
        svg_decode(s.as_bytes(), &Limits::DEFAULT),
        Err(DecodeError::LimitExceeded(_))
    ));
    // A use of its own ancestor, and mutually recursive uses, draw nothing.
    let cyc = format!(
        "<svg {NS} width='4' height='4'><g id='a'><use href='#a'/></g><g id='x'><use href='#y'/></g><g id='y'><use href='#x'/></g><use href='#x'/></svg>"
    );
    let img = render(&cyc);
    assert!(img.rgba.iter().all(|b| *b == 0));
    // Nesting deeper than the cap is refused at parse time, not by the stack.
    let deep = format!(
        "<svg {NS}>{}{}</svg>",
        "<g>".repeat(5000),
        "</g>".repeat(5000)
    );
    assert!(svg_decode(deep.as_bytes(), &Limits::DEFAULT).is_err());
}

#[test]
fn absurd_sizes_and_numbers_are_limits_not_panics() {
    let huge = format!("<svg {NS} width='100000' height='100000'/>");
    assert!(matches!(
        svg_decode(huge.as_bytes(), &Limits::DEFAULT),
        Err(DecodeError::LimitExceeded(_) | DecodeError::BadDimensions)
    ));
    let nan = format!(
        "<svg {NS} width='4' height='4' viewBox='0 0 1e999 -1e999'><rect width='1e308' height='-5' x='1e-999' rx='9e99'/>\
         <circle r='1e999' cx='1e999' stroke='red' stroke-width='1e999'/><path d='M1e999 1e999 L-1e999 5e99 A1e99 1e-99 1e99 1 1 1e99 1e99 Z' stroke='#000' stroke-width='1e40' stroke-dasharray='1e-9'/></svg>"
    );
    let _ = svg_decode(nan.as_bytes(), &Limits::DEFAULT);
    // A dash pattern that would cut a line into millions of pieces.
    let dash = format!(
        "<svg {NS} width='10' height='10'><path d='M0 5 H1000000' stroke='#000' stroke-dasharray='0.01 0.01'/></svg>"
    );
    assert!(svg_decode(dash.as_bytes(), &Limits::DEFAULT).is_err());
    // A transform that is not invertible must not break a gradient.
    let sing = format!(
        "<svg {NS} width='4' height='4'><linearGradient id='g' gradientTransform='scale(0)'><stop offset='0' stop-color='red'/><stop offset='1'/></linearGradient><rect width='4' height='4' fill='url(#g)'/></svg>"
    );
    assert!(svg_decode(sing.as_bytes(), &Limits::DEFAULT).is_ok());
    // The budget caps the canvas.
    let doc = Document::parse(format!("<svg {NS} width='100' height='100'/>").as_bytes()).unwrap();
    let tiny = Limits {
        max_pixels: 99,
        max_frames: 1,
        max_total_bytes: 1 << 20,
    };
    assert!(doc.render(100, 100, [0; 4], &tiny).is_err());
    let small_mem = Limits {
        max_pixels: 1 << 20,
        max_frames: 1,
        max_total_bytes: 1000,
    };
    assert!(doc.render(100, 100, [0; 4], &small_mem).is_err());
}

fn as_decoded(data: &[u8], limits: &Limits) -> Result<Decoded, DecodeError> {
    svg_decode(data, limits).map(Decoded::Still)
}

#[test]
fn hostile_battery_over_the_render_fixtures() {
    for (name, data) in [
        (
            "badge",
            &include_bytes!("../../../tests/render/pages/img/badge.svg")[..],
        ),
        (
            "chart",
            include_bytes!("../../../tests/render/pages/img/chart.svg"),
        ),
        (
            "paths",
            include_bytes!("../../../tests/render/pages/img/paths.svg"),
        ),
        (
            "tile",
            include_bytes!("../../../tests/render/pages/img/tile.svg"),
        ),
        (
            "hostile",
            include_bytes!("../../../tests/render/pages/img/hostile.svg"),
        ),
    ] {
        hostile_battery(name, data, &as_decoded);
    }
}

#[test]
fn random_bytes_behind_svg_prefixes_never_panic() {
    random_bytes_battery(
        b"<svg xmlns='http://www.w3.org/2000/svg' width='16' height='16'>",
        7,
        &as_decoded,
    );
    random_bytes_battery(b"<svg width='8' height='8'><path d='", 11, &as_decoded);
    random_bytes_battery(b"<svg width='8' height='8'><rect style='", 13, &as_decoded);
    random_bytes_battery(b"<svg width='8' height='8'><g transform='", 17, &as_decoded);
    random_bytes_battery(b"<svg width='8' height='8'><style>", 19, &as_decoded);
}

#[test]
fn mutated_attribute_values_never_panic() {
    // Splice nasty tokens into every attribute value of a rich document.
    let src = include_str!("../../../tests/render/pages/img/paths.svg");
    let nasty = [
        "",
        "-1",
        "1e999",
        "nan",
        "0",
        "#",
        "url(#",
        "1,2,3",
        "M",
        "a 1 1 0 2 2 1 1",
        "999999999999999999999999",
    ];
    let limits = small_limits();
    let mut count = 0;
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"'
            && let Some(end) = src[i + 1..].find('"')
        {
            for n in nasty {
                let mut s = String::new();
                s.push_str(&src[..=i]);
                s.push_str(n);
                s.push_str(&src[i + 1 + end..]);
                let r = as_decoded(s.as_bytes(), &limits);
                check_result(&r, &limits);
                count += 1;
            }
            i += end + 1;
        }
        i += 1;
    }
    assert!(count > 200, "{count}");
}

// -- used-size rendering and page style (ADR 051) ------------------------------

fn chain(items: &[(&str, &str, &str)]) -> Vec<(String, String, String)> {
    items
        .iter()
        .map(|(t, i, c)| (t.to_string(), i.to_string(), c.to_string()))
        .collect()
}

#[test]
fn ratio_only_means_viewbox_without_absolute_size() {
    let ro = |attrs: &str| {
        Document::parse(format!("<svg {NS} {attrs}/>").as_bytes())
            .unwrap()
            .is_ratio_only()
    };
    assert!(ro("viewBox='0 0 24 24'"));
    assert!(ro("width='100%' viewBox='0 0 24 24'"));
    assert!(!ro("width='24' viewBox='0 0 24 24'"));
    assert!(!ro("height='24' viewBox='0 0 24 24'"));
    assert!(!ro("width='24' height='24'"));
    assert!(!ro(""));
}

#[test]
fn rendering_larger_is_sharp_and_reruns_preserve_aspect_ratio() {
    let doc = Document::parse(
        format!(
            "<svg {NS} width='10' height='10' viewBox='0 0 10 10'><rect width='5' height='10'/></svg>"
        )
        .as_bytes(),
    )
    .unwrap();
    // 4x: the rect's edge is exactly at x = 20, with no blur.
    let big = doc
        .render(40, 40, [0, 0, 0, 255], &Limits::DEFAULT)
        .unwrap();
    assert_eq!(px(&big, 19, 20), [0, 0, 0, 255]);
    assert_eq!(px(&big, 20, 20)[3], 0);
    // A 40x20 box: meet, centred, so the rect covers x 10..20 only.
    let wide = doc
        .render(40, 20, [0, 0, 0, 255], &Limits::DEFAULT)
        .unwrap();
    assert_eq!(px(&wide, 9, 10)[3], 0);
    assert_eq!(px(&wide, 10, 10)[3], 255);
    assert_eq!(px(&wide, 19, 10)[3], 255);
    assert_eq!(px(&wide, 20, 10)[3], 0);
}

#[test]
fn page_style_reaches_inside_and_ranks_correctly() {
    // No context ancestors: `.icon path` cannot match, so attributes stand and
    // the document's own style still beats the page's.
    let mut doc = Document::parse(
        format!(
            "<svg {NS} width='4' height='4'><style>#own{{fill:#00f}}</style>\
             <path id='own' d='M0 0H2V2H0z' fill='#0f0'/><path d='M2 2H4V4H2z' fill='#0f0'/></svg>"
        )
        .as_bytes(),
    )
    .unwrap();
    doc.add_page_style(".icon path{fill:#f00} #own{fill:#fff}", &[]);
    let img = doc.render(4, 4, [0, 0, 0, 255], &Limits::DEFAULT).unwrap();
    assert_eq!(px(&img, 3, 3), [0, 255, 0, 255]);
    assert_eq!(px(&img, 0, 0), [0, 0, 255, 255], "own style beats the page");

    let mut doc = Document::parse(
        format!(
            "<svg {NS} width='4' height='4'><path d='M2 2H4V4H2z' fill='#0f0'/>\
             <path class='k' d='M0 0H2V2H0z'/></svg>"
        )
        .as_bytes(),
    )
    .unwrap();
    doc.add_page_style(
        ".icon path{fill:#f00} .k{fill:#00f} @media print{.icon path{fill:#000}} \
         a:hover path{fill:#000}",
        &chain(&[("span", "", "icon big"), ("body", "", "")]),
    );
    let img = doc.render(4, 4, [0, 0, 0, 255], &Limits::DEFAULT).unwrap();
    // A page rule beats a presentation attribute.
    assert_eq!(px(&img, 3, 3), [255, 0, 0, 255]);
    // `.k` (one class) is less specific than `.icon path` (class + type).
    assert_eq!(px(&img, 0, 0), [255, 0, 0, 255]);
}

#[test]
fn page_style_is_bounded() {
    let mut doc = Document::parse(
        format!("<svg {NS} width='2' height='2'><rect width='2' height='2'/></svg>").as_bytes(),
    )
    .unwrap();
    let huge = "rect{fill:red}".repeat(2_000_000);
    doc.add_page_style(&huge, &chain(&[("div", "", ""); 100]));
    let img = doc.render(2, 2, [0, 0, 0, 255], &Limits::DEFAULT).unwrap();
    assert_eq!(img.width, 2);
}
